//! Linux 上看有哪些显卡、能不能用 VA-API 硬件编码，用不了时说明原因和办法。
//!
//! 从 `/sys/class/drm/renderD*/device` 读厂商号、设备号和内核驱动（不依赖任何库，测试里用假的目录）。
//! - Intel（0x8086）：VA-API 编码，要装 `intel-media-va-driver`（新一些的核显，iHD）或 `i965-va-driver`（老核显）；
//! - AMD（0x1002）：VA-API 编码，要装 `mesa-va-drivers`；
//! - NVIDIA（0x10de）：VA-API 不能编码（NVIDIA 的硬件编码是 NVENC，以后接）；
//! - 没有渲染节点：多半是虚拟机或没有显卡。

use std::path::{Path, PathBuf};

/// 一块显卡（一个渲染节点）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gpu {
    /// 渲染节点，例如 `/dev/dri/renderD128`。
    pub node: PathBuf,
    /// PCI 厂商号。
    pub vendor: u16,
    /// PCI 设备号。
    pub device: u16,
    /// 内核驱动（i915、xe、amdgpu、nvidia、nouveau、virtio_gpu……）；读不到时为空。
    pub driver: String,
}

/// 厂商名。
#[must_use]
pub fn vendor_name(vendor: u16) -> &'static str {
    match vendor {
        0x8086 => "Intel",
        0x1002 => "AMD",
        0x10de => "NVIDIA",
        0x1af4 => "virtio（虚拟机）",
        0x15ad => "VMware（虚拟机）",
        _ => "其他",
    }
}

fn hex(text: &str) -> Option<u16> {
    u16::from_str_radix(text.trim().trim_start_matches("0x"), 16).ok()
}

/// 列出 `sys`（一般是 `/sys/class/drm`）下的渲染节点。读不了的跳过。按节点名排序。
#[must_use]
pub fn list(sys: &Path) -> Vec<Gpu> {
    let mut gpus: Vec<Gpu> = std::fs::read_dir(sys)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("renderD") {
                return None;
            }
            let device = entry.path().join("device");
            let read = |file: &str| std::fs::read_to_string(device.join(file)).ok();
            let driver = std::fs::read_link(device.join("driver"))
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_default();
            Some(Gpu {
                node: Path::new("/dev/dri").join(&name),
                vendor: hex(&read("vendor")?)?,
                device: read("device").and_then(|d| hex(&d)).unwrap_or(0),
                driver,
            })
        })
        .collect();
    gpus.sort_by(|a, b| a.node.cmp(&b.node));
    gpus
}

/// 一行描述，例如 `Intel 0x9a49（i915，/dev/dri/renderD128）`。
#[must_use]
pub fn describe(gpu: &Gpu) -> String {
    format!(
        "{} 0x{:04x}（{}，{}）",
        vendor_name(gpu.vendor),
        gpu.device,
        if gpu.driver.is_empty() {
            "驱动未知"
        } else {
            &gpu.driver
        },
        gpu.node.display()
    )
}

/// 哪块显卡适合用 VA-API 编码：Intel、AMD 优先（同时有核显和 NVIDIA 独显时用核显编码）。
#[must_use]
pub fn pick(gpus: &[Gpu]) -> Option<&Gpu> {
    gpus.iter().find(|g| matches!(g.vendor, 0x8086 | 0x1002))
}

/// VA-API 用不了时给用户的说明：原因和办法。`why` 是 VA-API 自己报的错（可以为空）。
#[must_use]
pub fn advice(gpus: &[Gpu], why: &str) -> String {
    let detail = if why.is_empty() {
        String::new()
    } else {
        format!("（{why}）")
    };
    let Some(first) = gpus.first() else {
        return format!(
            "没有找到显卡（/dev/dri 下没有渲染节点；虚拟机里通常是这样）{detail}，用软件编码"
        );
    };
    match pick(gpus) {
        Some(gpu) if gpu.vendor == 0x8086 => format!(
            "Intel 显卡 {} 的 VA-API 编码用不了{detail}：请安装 intel-media-va-driver（较新的核显）\
             或 i965-va-driver（较老的核显），并确认当前用户能读写 {}",
            describe(gpu),
            gpu.node.display()
        ),
        Some(gpu) => format!(
            "AMD 显卡 {} 的 VA-API 编码用不了{detail}：请安装 mesa-va-drivers，并确认当前用户能读写 {}",
            describe(gpu),
            gpu.node.display()
        ),
        None if first.vendor == 0x10de => format!(
            "NVIDIA 显卡 {}：VA-API 不支持 NVIDIA 的硬件编码（要用 NVENC，还没接），用软件编码",
            describe(first)
        ),
        None => format!(
            "显卡 {} 没有可用的硬件编码{detail}，用软件编码",
            describe(first)
        ),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn fake(root: &Path, node: &str, vendor: &str, device: &str, driver: Option<&str>) {
        let dev = root.join(node).join("device");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join("vendor"), format!("{vendor}\n")).unwrap();
        std::fs::write(dev.join("device"), format!("{device}\n")).unwrap();
        if let Some(driver) = driver {
            let target = root.join("drivers").join(driver);
            std::fs::create_dir_all(&target).unwrap();
            std::os::unix::fs::symlink(&target, dev.join("driver")).unwrap();
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("peerdesk-gpu-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lists_render_nodes_and_picks_intel_or_amd() {
        let root = temp("list");
        fake(&root, "renderD129", "0x10de", "0x2484", Some("nvidia"));
        fake(&root, "renderD128", "0x8086", "0x9a49", Some("i915"));
        fake(&root, "card0", "0x8086", "0x9a49", None); // 不是渲染节点
        fake(&root, "renderD130", "garbage", "0x1", None); // 读不懂：跳过
        let gpus = list(&root);
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].node, Path::new("/dev/dri/renderD128"));
        assert_eq!(gpus[0].driver, "i915");
        assert_eq!(pick(&gpus).unwrap().vendor, 0x8086, "有核显时用核显编码");
        assert!(describe(&gpus[0]).contains("Intel 0x9a49（i915"));
        assert!(advice(&gpus, "vaInitialize 失败").contains("intel-media-va-driver"));
        assert!(list(&root.join("没有")).is_empty());
    }

    #[test]
    fn advice_per_vendor() {
        let gpu = |vendor, driver: &str| Gpu {
            node: "/dev/dri/renderD128".into(),
            vendor,
            device: 1,
            driver: driver.into(),
        };
        assert!(advice(&[], "").contains("没有找到显卡"));
        assert!(advice(&[gpu(0x1002, "amdgpu")], "").contains("mesa-va-drivers"));
        assert!(advice(&[gpu(0x10de, "nvidia")], "").contains("NVENC"));
        assert!(advice(&[gpu(0x1af4, "")], "x").contains("没有可用的硬件编码（x）"));
        assert!(describe(&gpu(0x1af4, "")).contains("驱动未知"));
        for v in [0x8086, 0x1002, 0x10de, 0x1af4, 0x15ad, 0x1234] {
            assert!(!vendor_name(v).is_empty());
        }
    }
}
