//! macOS 剪贴板的延迟提供数据（系统胶水）：剪贴板项只声明类型，别的程序来取时才给数据。
//!
//! 依据（Apple Developer Documentation，
//! <https://developer.apple.com/documentation/appkit/nspasteboarditemdataprovider>）：
//! `NSPasteboardItem.setDataProvider(_:forTypes:)` 指定一个数据提供者，提供者实现
//! `pasteboard(_:item:provideDataForType:)`（原文：*Asks the receiver to provide data for a specified
//! type to a given pasteboard*），在里面对这个项 `setData` / `setString`。
//!
//! 文件：每个文件（或文件夹）一个剪贴板项，类型 `public.file-url`，被取时先把对方的文件收到临时目录，
//! 再给出这个文件的 `file://` 网址。没有用 `NSFilePromiseProvider`：它的文档说的是拖放
//! （*When you’re working with drag and drop, use promises…*），Finder 的“粘贴”是否接受文件承诺
//! 文档没有说，**没有在真机上验证**；`public.file-url` 是 Finder 复制文件时自己放的类型。
//! Finder 对“延迟提供的 `public.file-url`”是否照常粘贴，同样要真机确认。
//!
//! 系统的限制：回调期间来粘贴的程序在等（回调在哪个线程上、要不要本进程有主运行循环，文档没有写，
//! 要真机确认），所以调用方给的取数函数必须有超时。
//!
//! 不让剪贴板管理工具来取：它们会在内容一变时就把数据取走，等于复制时就传。每个项另放一个
//! `org.nspasteboard.TransientType`（剪贴板工具之间的约定，见 <http://nspasteboard.org>，
//! **没有核对原文**，也不是所有工具都遵守）。
//!
//! 这个文件只依赖 objc2 的绑定和标准库，和会话无关。只在 macOS 上编译。
#![allow(unsafe_code)]

use std::sync::{Arc, Mutex, PoisonError};

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_app_kit::{
    NSPasteboard, NSPasteboardItem, NSPasteboardItemDataProvider, NSPasteboardType,
    NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSData, NSString};

/// 文字。
pub const TEXT: &str = "public.utf8-plain-text";
/// PNG 图片。
pub const PNG: &str = "public.png";
/// 文件的网址。
pub const FILE_URL: &str = "public.file-url";
/// 给剪贴板管理工具看的标记：这一项是临时的，不要收。
const TRANSIENT: &str = "org.nspasteboard.TransientType";

/// 给系统的数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provided {
    /// 原始字节（图片）。
    Data(Vec<u8>),
    /// 字符串（文字、文件网址）。
    Text(String),
}

/// 有程序来取第几项的某个类型时调用；给不了时返回 `None`。会阻塞来取的那个程序。
pub type Provide = Arc<dyn Fn(usize, &str) -> Option<Provided> + Send + Sync>;
/// 数据交给系统以后调用（这时剪贴板的变化计数可能变了，调用方要重新记）。
pub type Settled = Arc<dyn Fn() + Send + Sync>;

struct Ivars {
    /// 这是第几个剪贴板项。
    index: usize,
    provide: Provide,
    settled: Settled,
}

define_class!(
    // SAFETY: NSObject 没有特殊的子类化要求；本类没有实现 Drop，实例变量随对象正常释放。
    #[unsafe(super(NSObject))]
    #[name = "PeerDeskClipProvider"]
    #[ivars = Ivars]
    struct Provider;

    unsafe impl NSObjectProtocol for Provider {}

    unsafe impl NSPasteboardItemDataProvider for Provider {
        #[unsafe(method(pasteboard:item:provideDataForType:))]
        fn provide(
            &self,
            _pasteboard: Option<&NSPasteboard>,
            item: &NSPasteboardItem,
            kind: &NSPasteboardType,
        ) {
            let ivars = self.ivars();
            match (ivars.provide)(ivars.index, &kind.to_string()) {
                Some(Provided::Data(bytes)) => {
                    let _ = item.setData_forType(&NSData::with_bytes(&bytes), kind);
                }
                Some(Provided::Text(text)) => {
                    let _ = item.setString_forType(&NSString::from_str(&text), kind);
                }
                // 给不了：什么都不放，来取的程序得到空结果
                None => {}
            }
            (ivars.settled)();
        }
    }
);

impl Provider {
    fn new(ivars: Ivars) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ivars);
        // SAFETY: 调用父类 NSObject 的 init，返回初始化好的对象。
        unsafe { msg_send![super(this), init] }
    }
}

/// 让提供者活到下一次声明：文档没有说剪贴板项会不会保留它的提供者。
struct Keep(#[allow(dead_code)] Vec<Retained<Provider>>);

// SAFETY: 这些对象放进来以后本进程不再访问它们，只在下一次声明时整体释放；
// 实例变量（序号和一个 Send + Sync 的闭包）可以在任何线程上释放。
unsafe impl Send for Keep {}

static KEEP: Mutex<Option<Keep>> = Mutex::new(None);

/// 清空剪贴板，放 `items` 个项，每项只声明类型 `kind`（不放数据）。之后有程序来取时调用 `provide`。
pub fn offer(items: usize, kind: &str, provide: &Provide, settled: &Settled) -> Result<(), String> {
    let board = NSPasteboard::generalPasteboard();
    let _ = board.clearContents();
    let types = NSArray::from_retained_slice(&[NSString::from_str(kind)]);
    let mut providers = Vec::new();
    let mut objects = Vec::new();
    for index in 0..items.max(1) {
        let provider = Provider::new(Ivars {
            index,
            provide: Arc::clone(provide),
            settled: Arc::clone(settled),
        });
        let item = NSPasteboardItem::new();
        if !item.setDataProvider_forTypes(ProtocolObject::from_ref(&*provider), &types) {
            return Err(format!("剪贴板项不接受类型 {kind}"));
        }
        let _ = item.setString_forType(&NSString::new(), &NSString::from_str(TRANSIENT));
        providers.push(provider);
        objects.push(ProtocolObject::<dyn NSPasteboardWriting>::from_retained(
            item,
        ));
    }
    let written = board.writeObjects(&NSArray::from_retained_slice(&objects));
    *KEEP.lock().unwrap_or_else(PoisonError::into_inner) = Some(Keep(providers));
    if written {
        Ok(())
    } else {
        Err("剪贴板不接受这些项".into())
    }
}
