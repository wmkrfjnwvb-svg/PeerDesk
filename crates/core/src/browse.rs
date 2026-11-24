//! 文件管理的协议（不做 IO）：浏览对端的目录、新建文件和文件夹、指定目录的上传和下载。
//!
//! 每个请求开一条“文件浏览”流：控制端写一条 [`Request`]，被控端回一条 [`Reply`]。
//! 上传（[`Request::Put`]）和下载（[`Request::Get`]）在回 [`Reply::Ready`] 之后，同一条流上接着跑
//! 文件传输协议（[`crate::file`]，带断点续传和哈希校验）：上传时被控端是接收方，根目录是请求里的目录；
//! 下载时被控端是发送方。
//!
//! 路径是对端系统的绝对路径（控制端已经能操作对端的键鼠，文件访问不额外放大权限；被控端可以关掉）。
//! 名字（新建、下载的条目）只能是一层，不能带路径分隔符，也不能是 `.`、`..`。
//! 每条消息前面有 4 字节长度（大端），上限 [`MAX_MSG`]。

use crate::wire::{Reader, Writer};
use crate::Error;

/// 一条消息的最大长度（目录列表可能很长）。
pub const MAX_MSG: usize = 4 * 1024 * 1024;
/// 路径的最大长度（字节）。
pub const MAX_PATH: usize = 4096;
/// 名字的最大长度（字节）。
pub const MAX_NAME: usize = 1024;
/// 一个目录最多列多少项（多了截断，并标记）。
pub const MAX_ENTRIES: usize = 20_000;
/// 一次下载最多几个条目。
pub const MAX_ITEMS: usize = 4096;
/// 原因的最大长度。
pub const MAX_REASON: usize = 300;

/// 预置位置（桌面、下载、磁盘……）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// 显示的名字（“桌面”“C:”）。
    pub label: String,
    /// 绝对路径。
    pub path: String,
}

/// 目录里的一项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 名字。
    pub name: String,
    /// 是不是目录。
    pub dir: bool,
    /// 大小（字节；目录为 0）。
    pub size: u64,
    /// 修改时间（Unix 秒；不知道时为 0）。
    pub modified: u64,
}

/// 控制端的请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// 预置位置和本机的用户目录。
    Places,
    /// 列目录。
    List {
        /// 绝对路径。
        path: String,
    },
    /// 新建文件夹。
    MakeDir {
        /// 所在目录。
        dir: String,
        /// 名字。
        name: String,
    },
    /// 新建空文件（已存在时失败，不覆盖）。
    MakeFile {
        /// 所在目录。
        dir: String,
        /// 名字。
        name: String,
    },
    /// 上传到这个目录。
    Put {
        /// 目标目录。
        dir: String,
    },
    /// 下载这个目录下的这些条目（文件或文件夹）。
    Get {
        /// 所在目录。
        dir: String,
        /// 条目名字。
        names: Vec<String>,
    },
}

/// 被控端的答复。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// 预置位置。第一个是用户主目录。
    Places(Vec<Place>),
    /// 目录列表。`truncated` 表示超过 [`MAX_ENTRIES`] 被截断了。
    List {
        /// 条目。
        entries: Vec<Entry>,
        /// 是否截断。
        truncated: bool,
    },
    /// 做完了（新建）。
    Done,
    /// 可以开始传了（上传、下载）。
    Ready,
    /// 拒绝或失败，带原因（给用户看）。
    Failed(String),
}

/// 检查一个名字：非空、一层、不是 `.`/`..`、没有控制字符。
///
/// # Errors
/// 名字不合法。
pub fn check_name(name: &str) -> Result<(), Error> {
    let bad = || Error::UnsafePath(name.to_owned());
    let invalid = name.is_empty()
        || name.len() > MAX_NAME
        || name == "."
        || name == ".."
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control);
    if invalid {
        return Err(bad());
    }
    Ok(())
}

fn count(len: usize, max: usize) -> Result<u32, Error> {
    if len > max {
        return Err(Error::TooManyItems { len, max });
    }
    u32::try_from(len).map_err(|_| Error::TooManyItems { len, max })
}

fn read_count(r: &mut Reader<'_>, max: usize) -> Result<usize, Error> {
    let len = usize::try_from(r.u32()?).unwrap_or(usize::MAX);
    if len > max {
        return Err(Error::TooManyItems { len, max });
    }
    Ok(len)
}

/// 加上 4 字节长度。
fn framed(body: Vec<u8>) -> Result<Vec<u8>, Error> {
    if body.len() > MAX_MSG {
        return Err(Error::PayloadTooLarge {
            len: body.len(),
            max: MAX_MSG,
        });
    }
    // 上面已经保证不超过 MAX_MSG（4 MB），装得进 u32
    let len = u32::try_from(body.len()).unwrap_or(u32::MAX);
    let mut out = len.to_be_bytes().to_vec();
    out.extend(body);
    Ok(out)
}

/// 从字节流里切出完整的消息体（4 字节长度 + 内容）。
#[derive(Debug, Default)]
pub struct Framer {
    buf: Vec<u8>,
}

impl Framer {
    /// 新的。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 加入收到的字节，返回凑齐的第一条消息体（一次只取一条：浏览流上一来一回）。
    ///
    /// # Errors
    /// 长度超过上限。
    pub fn push(&mut self, bytes: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        self.buf.extend_from_slice(bytes);
        let Some(head) = self.buf.get(..4) else {
            return Ok(None);
        };
        let mut len = [0u8; 4];
        len.copy_from_slice(head);
        let len = usize::try_from(u32::from_be_bytes(len)).unwrap_or(usize::MAX);
        if len > MAX_MSG {
            return Err(Error::PayloadTooLarge { len, max: MAX_MSG });
        }
        let Some(body) = self.buf.get(4..4 + len) else {
            return Ok(None);
        };
        let body = body.to_vec();
        self.buf.drain(..4 + len);
        Ok(Some(body))
    }

    /// 剩下还没用掉的字节（下载、上传时紧接着的文件传输消息）。
    #[must_use]
    pub fn into_rest(self) -> Vec<u8> {
        self.buf
    }
}

impl Request {
    /// 编码（带长度）。
    ///
    /// # Errors
    /// 路径、名字太长，条目太多。
    pub fn frame(&self) -> Result<Vec<u8>, Error> {
        let mut w = Writer::new();
        match self {
            Request::Places => {
                w.u8(1);
            }
            Request::List { path } => {
                w.u8(2).string16(path, MAX_PATH)?;
            }
            Request::MakeDir { dir, name } => {
                w.u8(3).string16(dir, MAX_PATH)?.string16(name, MAX_NAME)?;
            }
            Request::MakeFile { dir, name } => {
                w.u8(4).string16(dir, MAX_PATH)?.string16(name, MAX_NAME)?;
            }
            Request::Put { dir } => {
                w.u8(5).string16(dir, MAX_PATH)?;
            }
            Request::Get { dir, names } => {
                w.u8(6)
                    .string16(dir, MAX_PATH)?
                    .u32(count(names.len(), MAX_ITEMS)?);
                for name in names {
                    w.string16(name, MAX_NAME)?;
                }
            }
        }
        framed(w.into_vec())
    }

    /// 解码消息体（不带长度）。名字会检查。
    ///
    /// # Errors
    /// 格式不对、类型未知、名字不合法。
    pub fn decode(body: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(body);
        let req = match r.u8()? {
            1 => Request::Places,
            2 => Request::List {
                path: r.string16(MAX_PATH)?,
            },
            kind @ (3 | 4) => {
                let dir = r.string16(MAX_PATH)?;
                let name = r.string16(MAX_NAME)?;
                check_name(&name)?;
                if kind == 3 {
                    Request::MakeDir { dir, name }
                } else {
                    Request::MakeFile { dir, name }
                }
            }
            5 => Request::Put {
                dir: r.string16(MAX_PATH)?,
            },
            6 => {
                let dir = r.string16(MAX_PATH)?;
                let n = read_count(&mut r, MAX_ITEMS)?;
                let mut names = Vec::with_capacity(n.min(64));
                for _ in 0..n {
                    let name = r.string16(MAX_NAME)?;
                    check_name(&name)?;
                    names.push(name);
                }
                Request::Get { dir, names }
            }
            other => return Err(Error::UnknownMessage(other)),
        };
        r.finish()?;
        Ok(req)
    }
}

impl Reply {
    /// 编码（带长度）。
    ///
    /// # Errors
    /// 太长。
    pub fn frame(&self) -> Result<Vec<u8>, Error> {
        let mut w = Writer::new();
        match self {
            Reply::Places(places) => {
                w.u8(1).u32(count(places.len(), MAX_ITEMS)?);
                for p in places {
                    w.string16(&p.label, MAX_NAME)?
                        .string16(&p.path, MAX_PATH)?;
                }
            }
            Reply::List { entries, truncated } => {
                w.u8(2)
                    .u8(u8::from(*truncated))
                    .u32(count(entries.len(), MAX_ENTRIES)?);
                for e in entries {
                    w.string16(&e.name, MAX_NAME)?
                        .u8(u8::from(e.dir))
                        .u64(e.size)
                        .u64(e.modified);
                }
            }
            Reply::Done => {
                w.u8(3);
            }
            Reply::Ready => {
                w.u8(4);
            }
            Reply::Failed(why) => {
                let mut end = why.len().min(MAX_REASON);
                while !why.is_char_boundary(end) {
                    end -= 1;
                }
                w.u8(5)
                    .string16(why.get(..end).unwrap_or_default(), MAX_REASON)?;
            }
        }
        framed(w.into_vec())
    }

    /// 解码消息体（不带长度）。
    ///
    /// # Errors
    /// 格式不对、类型未知。
    pub fn decode(body: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(body);
        let reply = match r.u8()? {
            1 => {
                let n = read_count(&mut r, MAX_ITEMS)?;
                let mut places = Vec::with_capacity(n.min(64));
                for _ in 0..n {
                    places.push(Place {
                        label: r.string16(MAX_NAME)?,
                        path: r.string16(MAX_PATH)?,
                    });
                }
                Reply::Places(places)
            }
            2 => {
                let truncated = r.u8()? != 0;
                let n = read_count(&mut r, MAX_ENTRIES)?;
                let mut entries = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    entries.push(Entry {
                        name: r.string16(MAX_NAME)?,
                        dir: r.u8()? != 0,
                        size: r.u64()?,
                        modified: r.u64()?,
                    });
                }
                Reply::List { entries, truncated }
            }
            3 => Reply::Done,
            4 => Reply::Ready,
            5 => Reply::Failed(r.string16(MAX_REASON)?),
            other => return Err(Error::UnknownMessage(other)),
        };
        r.finish()?;
        Ok(reply)
    }
}

/// 目录列表的排序：文件夹在前，再按名字（不分大小写）。
pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn roundtrip_req(req: &Request) {
        let framed = req.frame().unwrap();
        let mut f = Framer::new();
        // 分两半送进去：凑齐了才出
        // 分三段送：长度不全、内容不全、凑齐
        let (a, rest) = framed.split_at(3);
        let (b, c) = rest.split_at(1);
        assert_eq!(f.push(a).unwrap(), None);
        assert_eq!(f.push(b).unwrap(), None);
        let body = f.push(c).unwrap().unwrap();
        assert_eq!(&Request::decode(&body).unwrap(), req);
    }

    #[test]
    fn requests_roundtrip() {
        for req in [
            Request::Places,
            Request::List {
                path: "C:\\Users\\李".into(),
            },
            Request::MakeDir {
                dir: "/home/u".into(),
                name: "新建文件夹".into(),
            },
            Request::MakeFile {
                dir: "/home/u".into(),
                name: "a.txt".into(),
            },
            Request::Put { dir: "/tmp".into() },
            Request::Get {
                dir: "/tmp".into(),
                names: vec!["a".into(), "b c".into()],
            },
        ] {
            roundtrip_req(&req);
        }
    }

    #[test]
    fn replies_roundtrip() {
        for reply in [
            Reply::Places(vec![Place {
                label: "桌面".into(),
                path: "/Users/u/Desktop".into(),
            }]),
            Reply::List {
                entries: vec![Entry {
                    name: "x".into(),
                    dir: true,
                    size: 0,
                    modified: 1_700_000_000,
                }],
                truncated: true,
            },
            Reply::Done,
            Reply::Ready,
            Reply::Failed("没有权限".into()),
        ] {
            let framed = reply.frame().unwrap();
            let body = Framer::new().push(&framed).unwrap().unwrap();
            assert_eq!(Reply::decode(&body).unwrap(), reply);
        }
        // 原因太长：截断到上限（按字符边界）
        let long = Reply::Failed("错".repeat(200)).frame().unwrap();
        let body = Framer::new().push(&long).unwrap().unwrap();
        assert!(
            matches!(Reply::decode(&body).unwrap(), Reply::Failed(why) if why.len() <= MAX_REASON)
        );
        // 截断点落在一个汉字中间：退到字符边界
        let odd = Reply::Failed(format!("a{}", "错".repeat(200)))
            .frame()
            .unwrap();
        let body = Framer::new().push(&odd).unwrap().unwrap();
        assert!(
            matches!(Reply::decode(&body).unwrap(), Reply::Failed(why) if why.len() == MAX_REASON - 2)
        );
    }

    #[test]
    fn names_are_one_level() {
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "x\u{0}",
            &"n".repeat(MAX_NAME + 1),
        ] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        assert!(check_name("报告 2026.docx").is_ok());
        // 解码时检查：带路径的名字被拒绝
        let evil = Request::Get {
            dir: "/".into(),
            names: vec!["../etc".into()],
        }
        .frame()
        .unwrap();
        assert!(Request::decode(evil.get(4..).unwrap()).is_err());
        let evil = Request::MakeDir {
            dir: "/".into(),
            name: "a/b".into(),
        }
        .frame()
        .unwrap();
        assert!(Request::decode(evil.get(4..).unwrap()).is_err());
    }

    #[test]
    fn malformed_and_limits() {
        assert!(Request::decode(&[9]).is_err());
        assert!(Reply::decode(&[9]).is_err());
        assert!(Request::decode(&[1, 0]).is_err(), "末尾多出字节");
        assert!(Request::decode(&[]).is_err());
        // 长度超过上限
        let mut f = Framer::new();
        assert!(f.push(&u32::MAX.to_be_bytes()).is_err());
        // 条目太多
        let names = vec!["a".to_owned(); MAX_ITEMS + 1];
        assert!(Request::Get {
            dir: "/".into(),
            names
        }
        .frame()
        .is_err());
        let mut body = vec![6u8];
        body.extend_from_slice(&1u16.to_be_bytes());
        body.push(b'/');
        body.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(Request::decode(&body).is_err());
        // 消息体太大
        assert!(framed(vec![0; MAX_MSG + 1]).is_err());
        assert!(count(usize::MAX, usize::MAX).is_err());
        // 剩下的字节交给文件协议
        let mut f = Framer::new();
        let mut bytes = Reply::Ready.frame().unwrap();
        bytes.extend_from_slice(b"rest");
        assert!(f.push(&bytes).unwrap().is_some());
        assert_eq!(f.into_rest(), b"rest");
    }

    #[test]
    fn sorting_puts_folders_first() {
        let e = |name: &str, dir| Entry {
            name: name.into(),
            dir,
            size: 0,
            modified: 0,
        };
        let mut list = vec![
            e("b.txt", false),
            e("Zeta", true),
            e("a.txt", false),
            e("alpha", true),
        ];
        sort_entries(&mut list);
        let names: Vec<&str> = list.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha", "Zeta", "a.txt", "b.txt"]);
    }
}
