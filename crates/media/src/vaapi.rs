//! Linux 硬件 H.264 编码（系统胶水）：VA-API，用 ChromeOS 的 cros-codecs（BSD-3）。
//!
//! - 显卡选择见 [`crate::gpu`]：Intel、AMD 走 VA-API；NVIDIA（VA-API 不能编码）、虚拟机说明原因后用软件。
//! - cros-codecs 的句柄（`Rc<Display>`）不能跨线程：编码器放在自己的线程里，用通道收发。
//! - 和 VideoToolbox、Media Foundation 一样是**异步**的：交帧不等，编好的帧随时取，可能攒帧；
//!   画面停下后排空。受约束的 Baseline（Windows 解码器能用显卡解）、恒定码率、只按需要出关键帧。
//!
//! 只在 Linux 上编译，覆盖率统计不含这个文件（要有显卡才能跑），由用户机器上的 `peerdesk hw-check` 验证。

use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use cros_codecs::backend::vaapi::encoder::VaapiBackend;
use cros_codecs::codec::h264::parser::{Level, Profile};
use cros_codecs::encoder::h264::EncoderConfig;
use cros_codecs::encoder::stateless::h264::StatelessEncoder;
use cros_codecs::encoder::{
    FrameMetadata, PredictionStructure, RateControl, Tunings, VideoEncoder,
};
use cros_codecs::libva::{self, Display, VAEntrypoint, VAProfile};
use cros_codecs::video_frame::gbm_video_frame::{GbmDevice, GbmUsage, GbmVideoFrame};
use cros_codecs::video_frame::VideoFrame;
use cros_codecs::{BlockingMode, Fourcc, FrameLayout, PlaneLayout, Resolution};

use crate::annexb;
use crate::gpu;

/// 交帧之后等编好的帧最多这么久（等不到下次再取）。
const OUTPUT_WAIT: Duration = Duration::from_millis(20);
/// 打开编码器最多等这么久。
const OPEN_WAIT: Duration = Duration::from_secs(5);
/// 排空最多等这么久。
const DRAIN_WAIT: Duration = Duration::from_millis(500);

type Memory = <GbmVideoFrame as VideoFrame>::MemDescriptor;
type Encoder = StatelessEncoder<GbmVideoFrame, VaapiBackend<Memory, libva::Surface<Memory>>>;

enum Job {
    Frame {
        nv12: Vec<u8>,
        bps: u64,
        force: bool,
    },
    Drain,
}

enum Out {
    Frame(Vec<u8>, bool),
    Failed(String),
    Drained,
}

/// 一个 VA-API 硬件编码器（编码在自己的线程上）。
pub struct VaapiEncoder {
    name: String,
    size: (u32, u32),
    jobs: Sender<Job>,
    out: Receiver<Out>,
    /// 排空时先收下来的帧（按顺序给 poll）。
    ready: std::collections::VecDeque<(Vec<u8>, bool)>,
}

impl std::fmt::Debug for VaapiEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaapiEncoder")
            .field("name", &self.name)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// 打开显卡（选 Intel、AMD 的渲染节点），确认支持受约束 Baseline 的编码。返回显示、显卡描述、是否低功耗入口。
fn open_display() -> Result<(Rc<Display>, std::path::PathBuf, String, bool), String> {
    let gpus = gpu::list(std::path::Path::new("/sys/class/drm"));
    let Some(chosen) = gpu::pick(&gpus) else {
        return Err(gpu::advice(&gpus, ""));
    };
    let display = Display::open_drm_display(&chosen.node)
        .map_err(|e| gpu::advice(&gpus, &format!("打开 {}：{e:?}", chosen.node.display())))?;
    let entrypoints = display
        .query_config_entrypoints(VAProfile::VAProfileH264ConstrainedBaseline)
        .map_err(|e| gpu::advice(&gpus, &format!("查询编码能力：{e:?}")))?;
    let low_power = entrypoints.contains(&VAEntrypoint::VAEntrypointEncSliceLP);
    if !low_power && !entrypoints.contains(&VAEntrypoint::VAEntrypointEncSlice) {
        return Err(gpu::advice(&gpus, "驱动不支持 H.264 编码"));
    }
    Ok((
        display,
        chosen.node.clone(),
        gpu::describe(chosen),
        low_power,
    ))
}

/// 16 对齐（编码器按宏块处理）。
fn align16(v: u32) -> u32 {
    v.div_ceil(16).saturating_mul(16)
}

/// 在显卡上分配一帧（GBM），把 NV12 按它的行宽拷进去。
fn upload(
    gbm: &Arc<GbmDevice>,
    size: Resolution,
    coded: Resolution,
    nv12: &[u8],
) -> Result<GbmVideoFrame, String> {
    let mut frame = Arc::clone(gbm)
        .new_frame(Fourcc::from(b"NV12"), size, coded, GbmUsage::Encode)
        .map_err(|e| format!("分配显卡帧：{e}"))?;
    let pitches = frame.get_plane_pitch();
    let (w, h) = (
        usize::try_from(size.width).unwrap_or(0),
        usize::try_from(size.height).unwrap_or(0),
    );
    {
        let mapping = frame.map_mut().map_err(|e| format!("映射显卡帧：{e}"))?;
        let planes = mapping.get();
        for (plane, rows, src_offset) in [(0usize, h, 0usize), (1usize, h / 2, w * h)] {
            let (Some(dst), Some(pitch)) = (planes.get(plane), pitches.get(plane)) else {
                return Err("显卡帧的平面数不对".into());
            };
            let mut dst = dst.borrow_mut();
            for y in 0..rows {
                let src = nv12
                    .get(src_offset + y * w..src_offset + (y + 1) * w)
                    .unwrap_or_default();
                if let Some(d) = dst.get_mut(y * pitch..y * pitch + src.len()) {
                    d.copy_from_slice(src);
                }
            }
        }
    }
    Ok(frame)
}

/// 编码线程：打开显卡和编码器，之后按任务编码，编好的帧送回去。
fn worker(
    width: u32,
    height: u32,
    fps: u32,
    bps: u64,
    jobs: &Receiver<Job>,
    out: &Sender<Out>,
    ready: &Sender<Result<String, String>>,
) {
    let opened = open_display().and_then(|(display, node, name, low_power)| {
        let coded = Resolution {
            width: align16(width),
            height: align16(height),
        };
        let config = EncoderConfig {
            resolution: Resolution { width, height },
            profile: Profile::Baseline,
            level: Level::L5_1,
            // 只在开始和我们要求时出关键帧（u16 上限）
            pred_structure: PredictionStructure::LowDelay { limit: u16::MAX },
            initial_tunings: Tunings {
                rate_control: RateControl::ConstantBitrate(bps),
                framerate: fps.max(1),
                ..Default::default()
            },
        };
        let encoder = Encoder::new_vaapi(
            Rc::clone(&display),
            config,
            Fourcc::from(b"NV12"),
            coded,
            low_power,
            BlockingMode::Blocking,
        )
        .map_err(|e| format!("打开 VA-API 编码器：{e:?}"))?;
        let gbm = GbmDevice::open(&node).map_err(|e| format!("打开 GBM：{e}"))?;
        Ok((encoder, gbm, coded, name, low_power))
    });
    let (mut encoder, gbm, coded, name, low_power) = match opened {
        Ok(parts) => parts,
        Err(why) => {
            let _ = ready.send(Err(why));
            return;
        }
    };
    let _ = ready.send(Ok(format!(
        "VA-API {name}{}",
        if low_power { "（低功耗）" } else { "" }
    )));
    let (w, h) = (
        usize::try_from(width).unwrap_or(0),
        usize::try_from(height).unwrap_or(0),
    );
    let stride = usize::try_from(width).unwrap_or(0);
    let layout = FrameLayout {
        format: (Fourcc::from(b"NV12"), 0),
        size: coded,
        planes: vec![
            PlaneLayout {
                buffer_index: 0,
                offset: 0,
                stride,
            },
            PlaneLayout {
                buffer_index: 0,
                offset: w * h,
                stride,
            },
        ],
    };
    let (mut timestamp, mut current_bps) = (0u64, bps);
    let send_all = |encoder: &mut Encoder| -> bool {
        loop {
            match encoder.poll() {
                Ok(Some(coded)) => {
                    let key = annexb::has_idr(&coded.bitstream);
                    if out.send(Out::Frame(coded.bitstream, key)).is_err() {
                        return false;
                    }
                }
                Ok(None) => return true,
                Err(e) => {
                    let _ = out.send(Out::Failed(format!("取编码结果：{e:?}")));
                    return false;
                }
            }
        }
    };
    while let Ok(job) = jobs.recv() {
        match job {
            Job::Frame { nv12, bps, force } => {
                // 码率变化超过一成才改（和其他编码器一样）
                if bps.abs_diff(current_bps).saturating_mul(10) > current_bps {
                    current_bps = bps;
                    let _ = encoder.tune(Tunings {
                        rate_control: RateControl::ConstantBitrate(bps),
                        framerate: fps.max(1),
                        ..Default::default()
                    });
                }
                let size = Resolution { width, height };
                let frame = match upload(&gbm, size, coded, &nv12) {
                    Ok(frame) => frame,
                    Err(why) => {
                        let _ = out.send(Out::Failed(why));
                        return;
                    }
                };
                timestamp += 1;
                let meta = FrameMetadata {
                    timestamp,
                    layout: layout.clone(),
                    force_keyframe: force,
                };
                if let Err(e) = encoder.encode(meta, frame) {
                    let _ = out.send(Out::Failed(format!("编码：{e:?}")));
                    return;
                }
            }
            Job::Drain => {
                if let Err(e) = encoder.drain() {
                    let _ = out.send(Out::Failed(format!("排空：{e:?}")));
                    return;
                }
                if !send_all(&mut encoder) {
                    return;
                }
                let _ = out.send(Out::Drained);
                continue;
            }
        }
        if !send_all(&mut encoder) {
            return;
        }
    }
}

impl VaapiEncoder {
    /// 打开（找显卡、确认能编码、建编码器）。用不了时返回原因和办法（给用户看）。
    pub fn open(width: u32, height: u32, fps: u32, bps: u64) -> Result<Self, String> {
        let (jobs, job_rx) = mpsc::channel();
        let (out_tx, out) = mpsc::channel();
        let (ready_tx, ready) = mpsc::channel();
        std::thread::Builder::new()
            .name("peerdesk-vaapi".into())
            .spawn(move || worker(width, height, fps, bps, &job_rx, &out_tx, &ready_tx))
            .map_err(|e| format!("起不了编码线程：{e}"))?;
        let name = ready
            .recv_timeout(OPEN_WAIT)
            .map_err(|_| "VA-API 编码器打开超时".to_owned())??;
        Ok(Self {
            name,
            size: (width, height),
            jobs,
            out,
            ready: std::collections::VecDeque::new(),
        })
    }

    /// 名称（含显卡）。
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 尺寸。
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        self.size
    }

    fn take(&mut self, wait: Duration) -> Result<Option<(Vec<u8>, bool)>, String> {
        if let Some(frame) = self.ready.pop_front() {
            return Ok(Some(frame));
        }
        let got = if wait.is_zero() {
            self.out.try_recv().ok()
        } else {
            self.out.recv_timeout(wait).ok()
        };
        match got {
            Some(Out::Frame(data, key)) => Ok(Some((data, key))),
            Some(Out::Failed(why)) => Err(why),
            Some(Out::Drained) | None => Ok(None),
        }
    }

    /// 交一帧 NV12，取回一个编好的帧（异步：可能是之前的，还没有时为 `None`）。
    pub fn encode(
        &mut self,
        nv12: &[u8],
        bps: u64,
        force: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, String> {
        self.jobs
            .send(Job::Frame {
                nv12: nv12.to_vec(),
                bps,
                force,
            })
            .map_err(|_| "VA-API 编码线程已经退出".to_owned())?;
        self.take(OUTPUT_WAIT)
    }

    /// 取走一个编好的帧（不等）。
    pub fn poll(&mut self) -> Result<Option<(Vec<u8>, bool)>, String> {
        self.take(Duration::ZERO)
    }

    /// 画面停下后让编码器交出攒着的帧（收进队列，之后用 [`Self::poll`] 取）。
    pub fn drain(&mut self) -> Result<(), String> {
        self.jobs
            .send(Job::Drain)
            .map_err(|_| "VA-API 编码线程已经退出".to_owned())?;
        let until = std::time::Instant::now() + DRAIN_WAIT;
        loop {
            let left = until.saturating_duration_since(std::time::Instant::now());
            match self.out.recv_timeout(left) {
                Ok(Out::Frame(data, key)) => self.ready.push_back((data, key)),
                Ok(Out::Failed(why)) => return Err(why),
                Ok(Out::Drained) | Err(_) => return Ok(()),
            }
        }
    }
}

impl crate::HardwareEncoder for VaapiEncoder {
    fn name(&self) -> &str {
        VaapiEncoder::name(self)
    }
    fn size(&self) -> (u32, u32) {
        VaapiEncoder::size(self)
    }
    fn encode(
        &mut self,
        nv12: &[u8],
        bps: u64,
        force_key: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, String> {
        VaapiEncoder::encode(self, nv12, bps, force_key)
    }
    fn poll(&mut self) -> Result<Option<(Vec<u8>, bool)>, String> {
        VaapiEncoder::poll(self)
    }
    fn drain(&mut self) -> Result<(), String> {
        VaapiEncoder::drain(self)
    }
}

// ───────────────────────────── 解码 ─────────────────────────────

use cros_codecs::backend::vaapi::decoder::VaapiBackend as VaapiDecodeBackend;
use cros_codecs::decoder::stateless::h264::H264;
use cros_codecs::decoder::stateless::{DecodeError, StatelessDecoder, StatelessVideoDecoder};
use cros_codecs::decoder::{DecodedHandle, DecoderEvent};

/// 解出的画面：宽、高、`0x00RRGGBB`。
type Picture = (usize, usize, Vec<u32>);

/// 一个 VA-API 硬件解码器（解码在自己的线程上：cros-codecs 的句柄不能跨线程）。
pub struct VaapiDecoder {
    name: String,
    jobs: Sender<Vec<u8>>,
    out: Receiver<Result<Option<Picture>, String>>,
}

impl std::fmt::Debug for VaapiDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaapiDecoder")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// 解码线程：每收到一帧码流，解出最新的画面（NV12 读回来转成 RGB）送回去。
fn decode_worker(
    jobs: &Receiver<Vec<u8>>,
    out: &Sender<Result<Option<Picture>, String>>,
    ready: &Sender<Result<String, String>>,
) {
    let opened = (|| -> Result<_, String> {
        let gpus = gpu::list(std::path::Path::new("/sys/class/drm"));
        let chosen = gpus.first().ok_or_else(|| gpu::advice(&gpus, ""))?;
        let display = Display::open_drm_display(&chosen.node)
            .map_err(|e| format!("打开 {}：{e:?}", chosen.node.display()))?;
        let entrypoints = display
            .query_config_entrypoints(VAProfile::VAProfileH264Main)
            .map_err(|e| format!("查询解码能力：{e:?}"))?;
        if !entrypoints.contains(&VAEntrypoint::VAEntrypointVLD) {
            return Err(format!(
                "显卡 {} 的驱动不支持 H.264 解码",
                gpu::describe(chosen)
            ));
        }
        let decoder = StatelessDecoder::<H264, VaapiDecodeBackend<GbmVideoFrame>>::new_vaapi(
            Rc::clone(&display),
            BlockingMode::Blocking,
        )
        .map_err(|e| format!("打开 VA-API 解码器：{e:?}"))?;
        let gbm = GbmDevice::open(&chosen.node).map_err(|e| format!("打开 GBM：{e}"))?;
        Ok((decoder, gbm, gpu::describe(chosen)))
    })();
    let (mut decoder, gbm, name) = match opened {
        Ok(parts) => parts,
        Err(why) => {
            let _ = ready.send(Err(why));
            return;
        }
    };
    let _ = ready.send(Ok(format!("VA-API {name}")));
    let mut sizes = (
        Resolution {
            width: 0,
            height: 0,
        },
        Resolution {
            width: 0,
            height: 0,
        },
    );
    let mut timestamp = 0u64;
    while let Ok(data) = jobs.recv() {
        timestamp += 1;
        let mut latest = None;
        let mut offset = 0usize;
        let result = loop {
            let rest = data.get(offset..).unwrap_or_default();
            if rest.is_empty() {
                break Ok(());
            }
            let (display_res, coded_res) = sizes;
            let gbm = Arc::clone(&gbm);
            let mut alloc = move || {
                Arc::clone(&gbm)
                    .new_frame(
                        Fourcc::from(b"NV12"),
                        display_res,
                        coded_res,
                        GbmUsage::Decode,
                    )
                    .ok()
            };
            match decoder.decode(timestamp, rest, &mut alloc) {
                Ok(used) => offset += used.max(1),
                // 要先处理事件（格式变了、要取走画面），处理完用同样的数据再试
                Err(DecodeError::CheckEvents | DecodeError::NotEnoughOutputBuffers(_)) => {}
                Err(e) => break Err(format!("解码：{e:?}")),
            }
            while let Some(event) = decoder.next_event() {
                match event {
                    DecoderEvent::FormatChanged => {
                        if let Some(info) = decoder.stream_info() {
                            sizes = (info.display_resolution, info.coded_resolution);
                        }
                    }
                    DecoderEvent::FrameReady(handle) => latest = Some(handle),
                }
            }
        };
        let picture = result.and_then(|()| {
            let Some(handle) = latest else {
                return Ok(None);
            };
            handle.sync().map_err(|e| format!("等解码完成：{e:?}"))?;
            let frame = handle.video_frame();
            let shown = handle.display_resolution();
            let pitches = frame.get_plane_pitch();
            let mapping = frame.map().map_err(|e| format!("读解出的画面：{e}"))?;
            let planes = mapping.get();
            let (Some(y), Some(uv), Some(pitch)) = (planes.first(), planes.get(1), pitches.first())
            else {
                return Err("解出的画面平面数不对".into());
            };
            let (w, h) = (
                usize::try_from(shown.width).unwrap_or(0),
                usize::try_from(shown.height).unwrap_or(0),
            );
            let mut rgb = Vec::new();
            crate::color::nv12_to_rgb(y, uv, *pitch, w, h, &mut rgb);
            Ok(Some((w, h, rgb)))
        });
        if out.send(picture).is_err() {
            return;
        }
    }
}

impl VaapiDecoder {
    /// 打开（找显卡、确认能解码）。用不了时返回原因。
    pub fn open() -> Result<Self, String> {
        let (jobs, job_rx) = mpsc::channel();
        let (out_tx, out) = mpsc::channel();
        let (ready_tx, ready) = mpsc::channel();
        std::thread::Builder::new()
            .name("peerdesk-vaapi-dec".into())
            .spawn(move || decode_worker(&job_rx, &out_tx, &ready_tx))
            .map_err(|e| format!("起不了解码线程：{e}"))?;
        let name = ready
            .recv_timeout(OPEN_WAIT)
            .map_err(|_| "VA-API 解码器打开超时".to_owned())??;
        Ok(Self { name, jobs, out })
    }
}

impl crate::HardwareDecoder for VaapiDecoder {
    fn name(&self) -> &str {
        &self.name
    }

    fn decode(&mut self, data: &[u8]) -> Result<Option<Picture>, String> {
        self.jobs
            .send(data.to_vec())
            .map_err(|_| "VA-API 解码线程已经退出".to_owned())?;
        self.out
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "VA-API 解码 2 秒没有结果".to_owned())?
    }
}
