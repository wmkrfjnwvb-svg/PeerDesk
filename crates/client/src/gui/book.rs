//! 设备簿：连接过的设备和用户建的分组（树形，可以嵌套），存在数据目录的 `devices.txt` 里。不存密码。
//!
//! 取代只记最近 8 条的 `recent_hosts.txt`（见 [`super::recent`]）：第一次运行时把老文件里的记录导入，
//! 老文件原样留着（退回老版本还能用），之后不再更新它。
//!
//! 以后要按账户在多台设备之间同步，所以数据结构先留好：
//! - 每个设备和分组有稳定的 id（随机 64 位），改名、移动都不变；
//! - 每条记录带修改时间（Unix 秒），同步时按“后改的赢”合并；
//! - 删除留下墓碑（id 和删除时间），否则另一台设备同步时会把删掉的东西又带回来。
//!
//! 文件每行一条，制表符分隔，第一个字段是记录种类：
//! - `D id 地址 指纹 系统 电脑名 别名 收藏 分组 上次连接 修改时间`
//! - `G id 上级分组 名称 折叠 修改时间`
//! - `X id 删除时间`
//!
//! 空字段写成 `-`。看不懂的行原样保留、写回去（新版本加了记录种类时，老版本不会把它们弄丢）。

use std::path::{Path, PathBuf};

use peerdesk_core::proto::PeerOs;

use super::recent::{os_from_key, os_key, RecentStore};

/// 文件名。
pub const FILE: &str = "devices.txt";
/// 文件第一行。
const HEADER: &str = "# peerdesk devices v1";
/// 分组名最多这么多个字。
pub const MAX_NAME: usize = 40;
/// 分组最多嵌套这么多层（再深，表格里的缩进就放不下名字了）。
pub const MAX_DEPTH: usize = 4;
/// 墓碑最多留这么多条（最老的先丢）。
const MAX_TOMBSTONES: usize = 500;

/// 设备或分组的稳定 id。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id(u64);

impl Id {
    /// 文件里的写法（16 位十六进制）。
    #[must_use]
    pub fn text(self) -> String {
        format!("{:016x}", self.0)
    }

    fn parse(text: &str) -> Option<Self> {
        u64::from_str_radix(text, 16).ok().map(Self)
    }

    /// 新的随机 id。用标准库哈希的随机种子：每次调用都不同，不会失败。
    fn random() -> Self {
        use std::hash::{BuildHasher as _, Hasher as _};
        Self(
            std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish(),
        )
    }
}

/// 一台设备。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// 稳定的 id。
    pub id: Id,
    /// 设备码或 `IP:端口`。
    pub addr: String,
    /// 核对过的指纹；没有时为空。
    pub pin: String,
    /// 对方的系统（不知道时为未知）。
    pub os: PeerOs,
    /// 对方的电脑名；不知道时为空。
    pub name: String,
    /// 用户给它起的名字；没起时为空（显示电脑名）。
    pub alias: String,
    /// 是否收藏。
    pub favorite: bool,
    /// 所在的分组；`None` 是未分组。
    pub group: Option<Id>,
    /// 上次连上的时间（Unix 秒）；没连过为 0。
    pub last_used: u64,
    /// 这条记录上次修改的时间（Unix 秒）。
    pub modified: u64,
}

impl Device {
    /// 设备码按“三位一组”显示，地址原样显示；以及是不是设备码。
    #[must_use]
    pub fn code(&self) -> (String, bool) {
        match peerdesk_core::signal::parse_code(&self.addr) {
            Some(code) if !self.addr.contains(':') => {
                (peerdesk_core::signal::format_code(code), true)
            }
            Some(_) | None => (self.addr.clone(), false),
        }
    }

    /// 显示的名字：别名，没有就用电脑名，再没有就用设备码或地址。
    #[must_use]
    pub fn title(&self) -> String {
        if !self.alias.is_empty() {
            self.alias.clone()
        } else if !self.name.is_empty() {
            self.name.clone()
        } else {
            self.code().0
        }
    }
}

/// 一个分组。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// 稳定的 id。
    pub id: Id,
    /// 上级分组；`None` 是顶层。
    pub parent: Option<Id>,
    /// 名称。
    pub name: String,
    /// 是否折叠（只是这台电脑上的显示状态，改它不算修改）。
    pub collapsed: bool,
    /// 这条记录上次修改的时间（Unix 秒）。
    pub modified: u64,
}

/// 操作失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookError {
    /// 名称是空的。
    EmptyName,
    /// 名称太长（超过 [`MAX_NAME`] 个字）。
    NameTooLong,
    /// 同一层已经有同名的分组。
    DuplicateName,
    /// 嵌套太深（超过 [`MAX_DEPTH`] 层）。
    TooDeep,
    /// 设备或分组不存在（可能刚被删掉）。
    NotFound,
    /// 写文件失败。
    Io(String),
}

/// 树形视图里的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// 分组。
    Group {
        /// 分组的 id。
        id: Id,
        /// 缩进层数（顶层是 0）。
        depth: usize,
        /// 名称。
        name: String,
        /// 里面的设备数（包括下级分组里的）。
        count: usize,
        /// 是否折叠。
        collapsed: bool,
    },
    /// “未分组”这一栏的标题（只在有分组、也有未分组的设备时出现）。
    Ungrouped {
        /// 未分组的设备数。
        count: usize,
    },
    /// 设备。
    Device {
        /// 设备的 id。
        id: Id,
        /// 缩进层数。
        depth: usize,
    },
}

/// 写进文件前去掉会破坏格式的字符（制表符、换行）。
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// 空字段在文件里写成 `-`。
fn field(s: &str) -> &str {
    if s.is_empty() {
        "-"
    } else {
        s
    }
}

/// 设备簿。
#[derive(Debug)]
pub struct Book {
    path: PathBuf,
    devices: Vec<Device>,
    groups: Vec<Group>,
    /// 墓碑：删掉的设备和分组的 id、删除时间。
    removed: Vec<(Id, u64)>,
    /// 看不懂的行，原样写回。
    unknown: Vec<String>,
}

impl Book {
    /// 读取数据目录里的设备簿。没有时从老的最近连接列表导入（`now` 是当前的 Unix 秒）；
    /// 读不了时从空的开始。看不懂的字段取默认值，指向不存在的分组的引用当作没有。
    #[must_use]
    pub fn load(dir: &Path, now: u64) -> Self {
        let path = dir.join(FILE);
        let mut book = Self {
            path,
            devices: Vec::new(),
            groups: Vec::new(),
            removed: Vec::new(),
            unknown: Vec::new(),
        };
        match std::fs::read_to_string(&book.path) {
            Ok(text) => book.parse(&text),
            Err(_) => book.import(&RecentStore::load(dir), now),
        }
        book
    }

    /// 导入老的最近连接列表（最近的在前）：按顺序给出递减的“上次连接”时间，保住先后。
    fn import(&mut self, recent: &RecentStore, now: u64) {
        for (i, r) in recent.items().iter().enumerate() {
            let age = u64::try_from(i).unwrap_or(u64::MAX);
            self.devices.push(Device {
                id: self.new_id(),
                addr: r.addr.clone(),
                pin: r.pin.clone(),
                os: r.os,
                name: r.name.clone(),
                alias: String::new(),
                favorite: false,
                group: None,
                last_used: now.saturating_sub(age).max(1),
                modified: now,
            });
        }
    }

    fn parse(&mut self, text: &str) {
        for line in text.lines() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.split('\t').collect();
            let get = |i: usize| match parts.get(i).copied().unwrap_or_default().trim() {
                "-" => "",
                other => other,
            };
            let number = |i: usize| get(i).parse::<u64>().unwrap_or(0);
            let id = Id::parse(get(1));
            let fresh = id.is_some_and(|id| {
                !self.devices.iter().any(|d| d.id == id) && !self.groups.iter().any(|g| g.id == id)
            });
            match (get(0), id) {
                ("D", Some(id)) if !get(2).is_empty() && fresh => {
                    // 同一个地址只留第一条
                    if self.devices.iter().any(|d| d.addr == get(2)) {
                        continue;
                    }
                    self.devices.push(Device {
                        id,
                        addr: get(2).to_owned(),
                        pin: get(3).to_owned(),
                        os: os_from_key(get(4)),
                        name: get(5).to_owned(),
                        alias: get(6).to_owned(),
                        favorite: get(7) == "1",
                        group: Id::parse(get(8)),
                        last_used: number(9),
                        modified: number(10),
                    });
                }
                ("G", Some(id)) if !get(3).is_empty() && fresh => self.groups.push(Group {
                    id,
                    parent: Id::parse(get(2)),
                    name: get(3).to_owned(),
                    collapsed: get(4) == "1",
                    modified: number(5),
                }),
                ("X", Some(id)) => self.removed.push((id, number(2))),
                // 重复或残缺的设备、分组行丢掉；其他种类的行是新版本写的，留着
                ("D" | "G", _) => {}
                _ => self.unknown.push(line.to_owned()),
            }
        }
        self.repair();
    }

    /// 修好读进来的引用：指向不存在的分组的，当作没有；上级关系成环的，断开放到顶层。
    fn repair(&mut self) {
        let ids: Vec<Id> = self.groups.iter().map(|g| g.id).collect();
        for d in &mut self.devices {
            d.group = d.group.filter(|g| ids.contains(g));
        }
        for g in &mut self.groups {
            g.parent = g.parent.filter(|p| ids.contains(p) && *p != g.id);
        }
        for id in ids {
            // 往上走的步数超过分组总数，说明绕回来了
            let mut at = Some(id);
            let mut steps = 0;
            while let Some(current) = at {
                at = self.group(current).and_then(|g| g.parent);
                steps += 1;
                if steps > self.groups.len() {
                    if let Some(g) = self.groups.iter_mut().find(|g| g.id == id) {
                        g.parent = None;
                    }
                    break;
                }
            }
        }
    }

    fn new_id(&self) -> Id {
        loop {
            let id = Id::random();
            let taken = self.devices.iter().any(|d| d.id == id)
                || self.groups.iter().any(|g| g.id == id)
                || self.removed.iter().any(|(r, _)| *r == id);
            if !taken {
                return id;
            }
        }
    }

    /// 全部设备（顺序不定，要排序的用 [`Self::recent`]、[`Self::favorites`]、[`Self::tree`]）。
    #[must_use]
    pub fn devices(&self) -> &[Device] {
        &self.devices
    }

    /// 全部分组。
    #[must_use]
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// 墓碑：删掉的设备和分组的 id、删除时间（同步用）。
    #[must_use]
    pub fn removed(&self) -> &[(Id, u64)] {
        &self.removed
    }

    /// 按 id 找设备。
    #[must_use]
    pub fn device(&self, id: Id) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == id)
    }

    /// 按地址找设备。
    #[must_use]
    pub fn device_at(&self, addr: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.addr == addr.trim())
    }

    /// 按 id 找分组。
    #[must_use]
    pub fn group(&self, id: Id) -> Option<&Group> {
        self.groups.iter().find(|g| g.id == id)
    }

    fn device_mut(&mut self, id: Id) -> Result<&mut Device, BookError> {
        self.devices
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or(BookError::NotFound)
    }

    fn group_mut(&mut self, id: Id) -> Result<&mut Group, BookError> {
        self.groups
            .iter_mut()
            .find(|g| g.id == id)
            .ok_or(BookError::NotFound)
    }

    /// 连过的设备，最近的在前，最多 `limit` 条。
    #[must_use]
    pub fn recent(&self, limit: usize) -> Vec<&Device> {
        let mut list: Vec<&Device> = self.devices.iter().filter(|d| d.last_used > 0).collect();
        list.sort_by(|a, b| b.last_used.cmp(&a.last_used).then(a.addr.cmp(&b.addr)));
        list.truncate(limit);
        list
    }

    /// 收藏的设备，按名字排。
    #[must_use]
    pub fn favorites(&self) -> Vec<&Device> {
        let mut list: Vec<&Device> = self.devices.iter().filter(|d| d.favorite).collect();
        list.sort_by_key(|d| (d.title().to_lowercase(), d.addr.clone()));
        list
    }

    /// 分组是第几层（顶层是 1）。读进来时环已经断开（见 `repair`），之后的操作不会造出环，往上走一定会停。
    fn depth(&self, id: Id) -> usize {
        let mut depth = 0;
        let mut at = Some(id);
        while let Some(current) = at {
            depth += 1;
            at = self.group(current).and_then(|g| g.parent);
        }
        depth
    }

    /// 分组的完整路径，例如“公司 › 设计组”。
    #[must_use]
    pub fn path(&self, id: Id) -> String {
        let mut names = Vec::new();
        let mut at = Some(id);
        while let Some(g) = at.and_then(|id| self.group(id)) {
            names.push(g.name.clone());
            at = g.parent;
        }
        names.reverse();
        names.join(" › ")
    }

    /// 某一层下面的分组，按名字排。
    fn children(&self, parent: Option<Id>) -> Vec<&Group> {
        let mut list: Vec<&Group> = self.groups.iter().filter(|g| g.parent == parent).collect();
        list.sort_by_key(|g| (g.name.to_lowercase(), g.id));
        list
    }

    /// 直接放在某个分组里（或未分组）的设备，按名字排。
    fn members(&self, group: Option<Id>) -> Vec<&Device> {
        let mut list: Vec<&Device> = self.devices.iter().filter(|d| d.group == group).collect();
        list.sort_by_key(|d| (d.title().to_lowercase(), d.addr.clone()));
        list
    }

    /// 分组里的设备数，包括下级分组里的。
    fn count(&self, group: Id) -> usize {
        self.members(Some(group)).len()
            + self
                .children(Some(group))
                .iter()
                .map(|g| self.count(g.id))
                .sum::<usize>()
    }

    /// 全部分组，按树的顺序（上级在前），带层数（顶层是 0）。“移到分组”的菜单用。
    #[must_use]
    pub fn group_list(&self) -> Vec<(Id, usize, String)> {
        let mut out = Vec::new();
        self.walk_groups(None, 0, &mut out);
        out
    }

    fn walk_groups(&self, parent: Option<Id>, depth: usize, out: &mut Vec<(Id, usize, String)>) {
        for g in self.children(parent) {
            out.push((g.id, depth, g.name.clone()));
            self.walk_groups(Some(g.id), depth + 1, out);
        }
    }

    /// 树形视图的各行：分组（折叠的不展开）、分组里的设备，最后是未分组的设备。
    #[must_use]
    pub fn tree(&self) -> Vec<Row> {
        let mut out = Vec::new();
        self.walk_tree(None, 0, &mut out);
        let loose = self.members(None);
        if !self.groups.is_empty() && !loose.is_empty() {
            out.push(Row::Ungrouped { count: loose.len() });
        }
        out.extend(loose.iter().map(|d| Row::Device { id: d.id, depth: 0 }));
        out
    }

    fn walk_tree(&self, parent: Option<Id>, depth: usize, out: &mut Vec<Row>) {
        for g in self.children(parent) {
            out.push(Row::Group {
                id: g.id,
                depth,
                name: g.name.clone(),
                count: self.count(g.id),
                collapsed: g.collapsed,
            });
            if g.collapsed {
                continue;
            }
            self.walk_tree(Some(g.id), depth + 1, out);
            out.extend(self.members(Some(g.id)).iter().map(|d| Row::Device {
                id: d.id,
                depth: depth + 1,
            }));
        }
    }

    /// 记下一次成功的连接并保存。没见过的地址新建一条；`os` 和 `name` 不知道时
    /// （对方是老版本）保留原来的值。
    pub fn remember(
        &mut self,
        addr: &str,
        pin: &str,
        os: PeerOs,
        name: &str,
        now: u64,
    ) -> Result<Id, BookError> {
        let addr = addr.trim();
        let id = match self.devices.iter().find(|d| d.addr == addr) {
            Some(d) => d.id,
            None => {
                let id = self.new_id();
                self.devices.push(Device {
                    id,
                    addr: addr.to_owned(),
                    pin: String::new(),
                    os: PeerOs::Unknown,
                    name: String::new(),
                    alias: String::new(),
                    favorite: false,
                    group: None,
                    last_used: 0,
                    modified: now,
                });
                id
            }
        };
        let d = self.device_mut(id)?;
        pin.trim().clone_into(&mut d.pin);
        if os != PeerOs::Unknown {
            d.os = os;
        }
        if !name.trim().is_empty() {
            d.name = clean(name);
        }
        d.last_used = now.max(1);
        d.modified = now;
        self.save()?;
        Ok(id)
    }

    /// 删掉一台设备（留下墓碑）。
    pub fn forget(&mut self, id: Id, now: u64) -> Result<(), BookError> {
        let before = self.devices.len();
        self.devices.retain(|d| d.id != id);
        if self.devices.len() == before {
            return Err(BookError::NotFound);
        }
        self.bury(id, now);
        self.save()
    }

    fn bury(&mut self, id: Id, now: u64) {
        self.removed.push((id, now));
        let extra = self.removed.len().saturating_sub(MAX_TOMBSTONES);
        drop(self.removed.drain(..extra));
    }

    /// 收藏或取消收藏。
    pub fn set_favorite(&mut self, id: Id, on: bool, now: u64) -> Result<(), BookError> {
        let d = self.device_mut(id)?;
        d.favorite = on;
        d.modified = now;
        self.save()
    }

    /// 给设备起名字；空的表示去掉别名，显示电脑名。
    pub fn rename_device(&mut self, id: Id, alias: &str, now: u64) -> Result<(), BookError> {
        let alias = clean(alias);
        if alias.chars().count() > MAX_NAME {
            return Err(BookError::NameTooLong);
        }
        let d = self.device_mut(id)?;
        d.alias = alias;
        d.modified = now;
        self.save()
    }

    /// 把设备移到分组里；`None` 是移到未分组。
    pub fn move_device(&mut self, id: Id, group: Option<Id>, now: u64) -> Result<(), BookError> {
        if group.is_some_and(|g| self.group(g).is_none()) {
            return Err(BookError::NotFound);
        }
        let d = self.device_mut(id)?;
        d.group = group;
        d.modified = now;
        self.save()
    }

    /// 检查分组名：去掉首尾空白和控制字符后不能空、不能太长、同一层不能重名。
    fn check_name(
        &self,
        name: &str,
        parent: Option<Id>,
        own: Option<Id>,
    ) -> Result<String, BookError> {
        let name = clean(name);
        if name.is_empty() {
            return Err(BookError::EmptyName);
        }
        if name.chars().count() > MAX_NAME {
            return Err(BookError::NameTooLong);
        }
        let taken = self.groups.iter().any(|g| {
            g.parent == parent && Some(g.id) != own && g.name.to_lowercase() == name.to_lowercase()
        });
        if taken {
            return Err(BookError::DuplicateName);
        }
        Ok(name)
    }

    /// 新建分组；`parent` 是上级分组，`None` 建在顶层。
    pub fn add_group(&mut self, parent: Option<Id>, name: &str, now: u64) -> Result<Id, BookError> {
        if let Some(p) = parent {
            if self.group(p).is_none() {
                return Err(BookError::NotFound);
            }
            if self.depth(p) >= MAX_DEPTH {
                return Err(BookError::TooDeep);
            }
        }
        let name = self.check_name(name, parent, None)?;
        let id = self.new_id();
        self.groups.push(Group {
            id,
            parent,
            name,
            collapsed: false,
            modified: now,
        });
        // 在折叠的分组里新建：展开它，新分组才看得见
        if let Some(p) = parent {
            self.group_mut(p)?.collapsed = false;
        }
        self.save()?;
        Ok(id)
    }

    /// 给分组改名。
    pub fn rename_group(&mut self, id: Id, name: &str, now: u64) -> Result<(), BookError> {
        let parent = self.group(id).ok_or(BookError::NotFound)?.parent;
        let name = self.check_name(name, parent, Some(id))?;
        let g = self.group_mut(id)?;
        g.name = name;
        g.modified = now;
        self.save()
    }

    /// 删除分组（留下墓碑）。不删设备：直接放在里面的设备回到未分组；
    /// 下级分组不跟着删，上移一层（接到被删分组的上级下面）。
    pub fn delete_group(&mut self, id: Id, now: u64) -> Result<(), BookError> {
        let parent = self.group(id).ok_or(BookError::NotFound)?.parent;
        self.groups.retain(|g| g.id != id);
        for d in self.devices.iter_mut().filter(|d| d.group == Some(id)) {
            d.group = None;
            d.modified = now;
        }
        for g in self.groups.iter_mut().filter(|g| g.parent == Some(id)) {
            g.parent = parent;
            g.modified = now;
        }
        self.bury(id, now);
        self.save()
    }

    /// 折叠或展开分组。只是这台电脑上的显示状态，不改修改时间。
    pub fn set_collapsed(&mut self, id: Id, collapsed: bool) -> Result<(), BookError> {
        self.group_mut(id)?.collapsed = collapsed;
        self.save()
    }

    /// 文件内容。
    fn text(&self) -> String {
        let mut out = format!("{HEADER}\n");
        let flag = |on: bool| if on { "1" } else { "0" };
        let link = |id: Option<Id>| id.map_or_else(|| "-".to_owned(), Id::text);
        for g in &self.groups {
            out.push_str(&format!(
                "G\t{}\t{}\t{}\t{}\t{}\n",
                g.id.text(),
                link(g.parent),
                g.name,
                flag(g.collapsed),
                g.modified
            ));
        }
        for d in &self.devices {
            out.push_str(&format!(
                "D\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                d.id.text(),
                d.addr,
                field(&d.pin),
                os_key(d.os),
                field(&d.name),
                field(&d.alias),
                flag(d.favorite),
                link(d.group),
                d.last_used,
                d.modified
            ));
        }
        for (id, at) in &self.removed {
            out.push_str(&format!("X\t{}\t{at}\n", id.text()));
        }
        for line in &self.unknown {
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    /// 保存：先写临时文件再改名，中途断电不会留下半个文件。
    fn save(&self) -> Result<(), BookError> {
        let io = |e: std::io::Error| BookError::Io(format!("{}：{e}", self.path.display()));
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let tmp = self.path.with_extension("txt.tmp");
        std::fs::write(&tmp, self.text()).map_err(io)?;
        std::fs::rename(&tmp, &self.path).map_err(io)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("peerdesk-book-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn titles(book: &Book) -> Vec<String> {
        book.tree()
            .into_iter()
            .map(|row| match row {
                Row::Group {
                    depth,
                    name,
                    count,
                    collapsed,
                    ..
                } => format!(
                    "{}[{name}:{count}{}]",
                    "  ".repeat(depth),
                    if collapsed { " 折叠" } else { "" }
                ),
                Row::Ungrouped { count } => format!("[未分组:{count}]"),
                Row::Device { id, depth } => {
                    format!("{}{}", "  ".repeat(depth), book.device(id).unwrap().title())
                }
            })
            .collect()
    }

    #[test]
    fn remembers_devices_and_keeps_what_an_old_peer_cannot_tell() {
        let dir = dir("remember");
        let mut book = Book::load(&dir, 100);
        assert!(book.devices().is_empty() && book.tree().is_empty());
        let a = book
            .remember(" 339166837 ", "ab12", PeerOs::Windows, "办公室\tPC", 100)
            .unwrap();
        let b = book
            .remember("192.168.1.20:4433", "", PeerOs::Unknown, "", 200)
            .unwrap();
        assert_ne!(a, b);
        // 再连一次同一台：还是那条记录；对方没报系统和名字时保留原来的
        let again = book
            .remember("339166837", "cd34", PeerOs::Unknown, " ", 300)
            .unwrap();
        assert_eq!(again, a);
        let d = book.device(a).unwrap();
        assert_eq!((d.os, d.name.as_str()), (PeerOs::Windows, "办公室 PC"));
        assert_eq!(
            (d.pin.as_str(), d.last_used, d.modified),
            ("cd34", 300, 300)
        );
        assert_eq!(d.code(), ("339 166 837".into(), true));
        assert_eq!(d.title(), "办公室 PC");
        let ip = book.device(b).unwrap();
        assert_eq!(ip.code(), ("192.168.1.20:4433".into(), false));
        assert_eq!(ip.title(), "192.168.1.20:4433");
        assert_eq!(book.device_at(" 339166837").unwrap().id, a);
        assert!(book.device_at("1").is_none());
        // 最近的在前，可以限制条数
        let recent: Vec<Id> = book.recent(8).iter().map(|d| d.id).collect();
        assert_eq!(recent, vec![a, b]);
        assert_eq!(book.recent(1).len(), 1);
        // 重新读出来一样
        let loaded = Book::load(&dir, 999);
        assert_eq!(loaded.devices(), book.devices());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn favorites_aliases_and_forgetting() {
        let dir = dir("favorite");
        let mut book = Book::load(&dir, 1);
        let a = book
            .remember("111111111", "", PeerOs::Mac, "mini", 1)
            .unwrap();
        let b = book
            .remember("222222222", "", PeerOs::Linux, "NAS", 2)
            .unwrap();
        book.set_favorite(a, true, 10).unwrap();
        book.set_favorite(b, true, 11).unwrap();
        book.rename_device(a, " 家里的 Mac ", 12).unwrap();
        assert_eq!(book.device(a).unwrap().title(), "家里的 Mac");
        assert_eq!(book.device(a).unwrap().modified, 12);
        let names: Vec<String> = book.favorites().iter().map(|d| d.title()).collect();
        assert_eq!(names, vec!["NAS", "家里的 Mac"]);
        // 去掉别名：显示电脑名
        book.rename_device(a, "", 13).unwrap();
        assert_eq!(book.device(a).unwrap().title(), "mini");
        assert_eq!(
            book.rename_device(a, &"长".repeat(MAX_NAME + 1), 14),
            Err(BookError::NameTooLong)
        );
        book.set_favorite(b, false, 15).unwrap();
        assert_eq!(book.favorites().len(), 1);
        // 删掉：留下墓碑，再删一次说不存在
        book.forget(b, 20).unwrap();
        assert!(book.device(b).is_none());
        assert_eq!(book.removed(), &[(b, 20)]);
        assert_eq!(book.forget(b, 21), Err(BookError::NotFound));
        assert_eq!(book.set_favorite(b, true, 21), Err(BookError::NotFound));
        assert_eq!(book.rename_device(b, "x", 21), Err(BookError::NotFound));
        assert_eq!(book.move_device(b, None, 21), Err(BookError::NotFound));
        let loaded = Book::load(&dir, 0);
        assert_eq!(loaded.removed(), &[(b, 20)]);
        assert_eq!(loaded.devices(), book.devices());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn groups_nest_and_devices_move_between_them() {
        let dir = dir("groups");
        let mut book = Book::load(&dir, 1);
        let pc = book
            .remember("111111111", "", PeerOs::Windows, "办公室 PC", 1)
            .unwrap();
        let mac = book
            .remember("222222222", "", PeerOs::Mac, "设计 Mac", 2)
            .unwrap();
        let nas = book
            .remember("333333333", "", PeerOs::Linux, "NAS", 3)
            .unwrap();
        // 没有分组时就是一张平的表，没有“未分组”标题
        assert_eq!(titles(&book), vec!["NAS", "办公室 PC", "设计 Mac"]);
        let company = book.add_group(None, " 公司 ", 10).unwrap();
        let design = book.add_group(Some(company), "设计组", 11).unwrap();
        let home = book.add_group(None, "家里", 12).unwrap();
        book.move_device(pc, Some(company), 20).unwrap();
        book.move_device(mac, Some(design), 21).unwrap();
        assert_eq!(book.device(mac).unwrap().modified, 21);
        assert_eq!(
            titles(&book),
            vec![
                "[公司:2]",
                "  [设计组:1]",
                "    设计 Mac",
                "  办公室 PC",
                "[家里:0]",
                "[未分组:1]",
                "NAS",
            ]
        );
        assert_eq!(book.path(design), "公司 › 设计组");
        assert_eq!(book.path(home), "家里");
        let list: Vec<(usize, String)> = book
            .group_list()
            .into_iter()
            .map(|(_, depth, name)| (depth, name))
            .collect();
        assert_eq!(
            list,
            vec![(0, "公司".into()), (1, "设计组".into()), (0, "家里".into())]
        );
        // 折叠：里面的不显示，数量还在；折叠不算修改
        book.set_collapsed(company, true).unwrap();
        assert_eq!(
            titles(&book),
            vec!["[公司:2 折叠]", "[家里:0]", "[未分组:1]", "NAS"]
        );
        assert_eq!(book.group(company).unwrap().modified, 10);
        // 在折叠的分组里新建下级：自动展开
        let ops = book.add_group(Some(company), "运维", 30).unwrap();
        assert!(!book.group(company).unwrap().collapsed);
        // 移回未分组；移到不存在的分组不行
        book.move_device(mac, None, 31).unwrap();
        assert_eq!(book.device(mac).unwrap().group, None);
        book.delete_group(ops, 32).unwrap();
        assert_eq!(
            book.move_device(mac, Some(ops), 33),
            Err(BookError::NotFound)
        );
        assert_eq!(book.set_collapsed(ops, true), Err(BookError::NotFound));
        // 全都移进分组后没有“未分组”这一栏
        book.move_device(mac, Some(design), 34).unwrap();
        book.move_device(nas, Some(home), 35).unwrap();
        assert!(!book.tree().contains(&Row::Ungrouped { count: 0 }));
        assert!(titles(&book).iter().all(|t| !t.contains("未分组")));
        // 读写一遍不变
        let loaded = Book::load(&dir, 0);
        assert_eq!(loaded.groups(), book.groups());
        assert_eq!(titles(&loaded), titles(&book));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn group_names_are_checked() {
        let dir = dir("names");
        let mut book = Book::load(&dir, 1);
        let company = book.add_group(None, "公司", 1).unwrap();
        assert_eq!(book.add_group(None, "  ", 2), Err(BookError::EmptyName));
        assert_eq!(
            book.add_group(None, &"长".repeat(MAX_NAME + 1), 2),
            Err(BookError::NameTooLong)
        );
        assert!(book.add_group(None, &"长".repeat(MAX_NAME), 2).is_ok());
        // 同一层不能重名（不分大小写），不同层可以
        let team = book.add_group(None, "Team", 2).unwrap();
        assert_eq!(
            book.add_group(None, "team", 3),
            Err(BookError::DuplicateName)
        );
        assert!(book.add_group(Some(company), "team", 3).is_ok());
        // 改名：改成自己现在的名字可以，改成兄弟的名字不行
        book.rename_group(team, "TEAM", 4).unwrap();
        assert_eq!(book.group(team).unwrap().name, "TEAM");
        assert_eq!(book.group(team).unwrap().modified, 4);
        assert_eq!(
            book.rename_group(team, "公司", 5),
            Err(BookError::DuplicateName)
        );
        assert_eq!(book.rename_group(team, "", 5), Err(BookError::EmptyName));
        // 最多嵌套 MAX_DEPTH 层
        let mut at = company;
        for depth in 2..=MAX_DEPTH {
            at = book
                .add_group(Some(at), &format!("第 {depth} 层"), 6)
                .unwrap();
        }
        assert_eq!(book.add_group(Some(at), "太深", 7), Err(BookError::TooDeep));
        // 不存在的分组
        book.delete_group(team, 8).unwrap();
        assert_eq!(book.add_group(Some(team), "x", 9), Err(BookError::NotFound));
        assert_eq!(book.rename_group(team, "x", 9), Err(BookError::NotFound));
        assert_eq!(book.delete_group(team, 9), Err(BookError::NotFound));
        assert_eq!(book.path(team), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_a_group_keeps_its_devices_and_lifts_its_subgroups() {
        let dir = dir("delete");
        let mut book = Book::load(&dir, 1);
        let pc = book
            .remember("111111111", "", PeerOs::Windows, "PC", 1)
            .unwrap();
        let mac = book
            .remember("222222222", "", PeerOs::Mac, "Mac", 2)
            .unwrap();
        let top = book.add_group(None, "公司", 3).unwrap();
        let mid = book.add_group(Some(top), "设计组", 4).unwrap();
        let low = book.add_group(Some(mid), "外包", 5).unwrap();
        book.move_device(pc, Some(mid), 6).unwrap();
        book.move_device(mac, Some(low), 7).unwrap();
        book.delete_group(mid, 50).unwrap();
        // 设备还在，回到未分组；下级分组上移一层，里面的设备不动
        assert_eq!(book.device(pc).unwrap().group, None);
        assert_eq!(book.device(pc).unwrap().modified, 50);
        assert_eq!(book.group(low).unwrap().parent, Some(top));
        assert_eq!(book.device(mac).unwrap().group, Some(low));
        assert_eq!(
            titles(&book),
            vec!["[公司:1]", "  [外包:1]", "    Mac", "[未分组:1]", "PC"]
        );
        assert_eq!(book.removed(), &[(mid, 50)]);
        // 删顶层分组：下级分组变成顶层
        book.delete_group(top, 60).unwrap();
        assert_eq!(book.group(low).unwrap().parent, None);
        assert_eq!(book.devices().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn imports_the_old_recent_list_once() {
        let dir = dir("import");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(super::super::recent::FILE),
            "339166837\t-\twindows\t办公室 PC\n192.168.1.20:4433 ab12\n",
        )
        .unwrap();
        let mut book = Book::load(&dir, 1000);
        let recent = book.recent(8);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].name, "办公室 PC");
        assert_eq!((recent[0].os, recent[0].last_used), (PeerOs::Windows, 1000));
        assert_eq!((recent[1].pin.as_str(), recent[1].last_used), ("ab12", 999));
        assert_ne!(recent[0].id, recent[1].id);
        // 存下来之后以设备簿为准，老文件原样留着
        let id = recent[0].id;
        book.set_favorite(id, true, 1001).unwrap();
        std::fs::write(dir.join(super::super::recent::FILE), "777777777\n").unwrap();
        let loaded = Book::load(&dir, 2000);
        assert_eq!(loaded.devices().len(), 2);
        assert!(loaded.device(id).unwrap().favorite);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tolerates_damaged_and_future_files() {
        let dir = dir("damaged");
        std::fs::create_dir_all(&dir).unwrap();
        let text = [
            HEADER,
            "",
            // 分组：b 的上级不存在；c 和 d 互为上级（环）；e 的上级是自己
            "G\t000000000000000a\t-\t公司\t1\t5",
            "G\t000000000000000b\t00000000000000ff\t孤儿\t0\t5",
            "G\t000000000000000c\t000000000000000d\t环一\t0\t5",
            "G\t000000000000000d\t000000000000000c\t环二\t0\t5",
            "G\t000000000000000e\t000000000000000e\t自己\t0\t5",
            // 重复的 id、没有名字、id 读不懂：丢掉
            "G\t000000000000000a\t-\t重复\t0\t5",
            "G\t000000000000000f\t-\t-\t0\t5",
            "G\tzz\t-\t坏 id\t0\t5",
            // 设备：字段不全的取默认值；分组不存在的当未分组；地址重复的、没有地址的丢掉
            "D\t0000000000000001\t111111111",
            "D\t0000000000000002\t222222222\t-\tmacos\tMac\t我的 Mac\t1\t000000000000000a\t7\t8",
            "D\t0000000000000003\t333333333\t-\t-\t-\t-\t0\t00000000000000ff\tx\ty",
            "D\t0000000000000004\t111111111",
            "D\t0000000000000005\t-",
            "X\t0000000000000009\t3",
            // 以后的版本加的记录：留着，写回去
            "S\tsync-cursor\t42",
        ]
        .join("\n");
        std::fs::write(dir.join(FILE), text).unwrap();
        let mut book = Book::load(&dir, 0);
        assert_eq!(book.groups().len(), 5);
        let parent = |n: u64| book.group(Id(n)).unwrap().parent;
        assert_eq!(parent(0xb), None);
        assert_eq!(parent(0xe), None);
        // 环被断开：至少有一个回到顶层，往上走不会绕圈
        assert!(parent(0xc).is_none() || parent(0xd).is_none());
        assert!(book.path(Id(0xc)).chars().count() < 20);
        assert!(book.group(Id(0xa)).unwrap().collapsed);
        assert_eq!(book.devices().len(), 3);
        let d1 = book.device(Id(1)).unwrap();
        assert_eq!(
            (d1.os, d1.favorite, d1.last_used),
            (PeerOs::Unknown, false, 0)
        );
        let d2 = book.device(Id(2)).unwrap();
        assert_eq!(d2.title(), "我的 Mac");
        assert_eq!((d2.group, d2.last_used, d2.modified), (Some(Id(0xa)), 7, 8));
        assert_eq!(book.device(Id(3)).unwrap().group, None);
        assert_eq!(book.device(Id(3)).unwrap().last_used, 0);
        // 没连过的不在“最近”里
        assert_eq!(book.recent(8).len(), 1);
        assert_eq!(book.removed(), &[(Id(9), 3)]);
        // 保存后，看不懂的那行还在
        book.set_favorite(Id(1), true, 9).unwrap();
        let saved = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(saved.starts_with(HEADER));
        assert!(saved.contains("S\tsync-cursor\t42\n"));
        assert_eq!(Id(0xa).text(), "000000000000000a");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_tombstones_are_dropped_and_write_errors_are_reported() {
        let dir = dir("tombstones");
        let mut book = Book::load(&dir, 0);
        for i in 0..=MAX_TOMBSTONES {
            book.bury(Id(u64::try_from(i).unwrap()), 1);
        }
        assert_eq!(book.removed().len(), MAX_TOMBSTONES);
        assert_eq!(book.removed()[0].0, Id(1));
        // 数据目录的位置上是个文件：保存失败，说明原因
        std::fs::write(&dir, b"").unwrap();
        let err = book.remember("111111111", "", PeerOs::Unknown, "", 1);
        assert!(matches!(err, Err(BookError::Io(_))));
        let _ = std::fs::remove_file(&dir);
        // 目录在，但目标文件的位置上是个目录：改名失败
        let mut book = Book::load(&dir, 0);
        std::fs::create_dir_all(dir.join(FILE)).unwrap();
        assert!(matches!(
            book.add_group(None, "公司", 1),
            Err(BookError::Io(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
