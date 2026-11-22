//! 真实声卡（系统胶水）：控制端播放、被控端采集“本机正在播放的声音”。用 cpal。
//!
//! - 播放：默认输出设备。
//! - 采集系统声音：
//!   - Windows：WASAPI 回环（在默认输出设备上开输入流，cpal 自动用回环模式）；
//!   - Linux：PulseAudio/PipeWire 的“默认输出的监听源”（`@DEFAULT_MONITOR@`），经 ALSA 的 pulse 插件；
//!   - macOS：系统不允许普通程序直接录系统声音，用 ScreenCaptureKit（macOS 13 起，需要“屏幕录制”权限），
//!     不录本程序自己的声音（避免回声）。
//!
//! 测试构建不会打开声卡（不碰开发者电脑的声卡），在各平台实机上验证。

use std::sync::{Arc, Mutex, PoisonError};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use peerdesk_net::audio::{AudioCapture, AudioPlayback, CaptureHandle, PlaybackHandle};
use tokio::sync::mpsc;

use crate::audio::{PlayQueue, ToFrames};

/// 播放队列最多积多少毫秒（超过就丢最老的，延迟不会越积越多）。
const MAX_QUEUE_MS: u32 = 120;

/// cpal 的流不能跨线程移动（部分平台），放在自己的线程里，丢弃句柄时线程结束。
struct StreamThread {
    stop: Option<std::sync::mpsc::Sender<()>>,
}

impl Drop for StreamThread {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// 在新线程里建流并让它一直跑，直到返回的句柄被丢弃。建流失败时返回原因。
fn run_stream(
    name: &str,
    build: impl FnOnce() -> Result<cpal::Stream, String> + Send + 'static,
) -> Result<StreamThread, String> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let stream = match build().and_then(|s| s.play().map(|()| s).map_err(|e| e.to_string()))
            {
                Ok(stream) => {
                    let _ = ready_tx.send(Ok(()));
                    stream
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            // 等到句柄被丢弃（发送端关闭）
            let _ = stop_rx.recv();
            drop(stream);
        })
        .map_err(|e| format!("启动声音线程失败：{e}"))?;
    ready_rx
        .recv()
        .map_err(|_| "声音线程意外退出".to_owned())??;
    Ok(StreamThread {
        stop: Some(stop_tx),
    })
}

/// 控制端：本机默认输出设备。第一次播放时才打开声卡（声音默认关，不打开就不占声卡）。
pub struct SystemPlayback {
    queue: Arc<Mutex<Option<PlayQueue>>>,
    stream: Mutex<Option<Result<StreamThread, String>>>,
}

impl std::fmt::Debug for SystemPlayback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SystemPlayback")
    }
}

impl SystemPlayback {
    fn open(&self) -> Result<StreamThread, String> {
        let queue = Arc::clone(&self.queue);
        run_stream("peerdesk-audio-out", move || {
            let device = cpal::default_host()
                .default_output_device()
                .ok_or_else(|| "这台电脑没有声音输出设备".to_owned())?;
            let config = device
                .default_output_config()
                .map_err(|e| format!("读不到声卡格式：{e}"))?;
            let (rate, channels) = (config.sample_rate().0, config.channels());
            *queue.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(PlayQueue::new(rate, channels, MAX_QUEUE_MS));
            peerdesk_net::note!(
                info,
                "声音：播放设备 {} {rate} Hz {channels} 声道",
                device.name().unwrap_or_default()
            );
            let cb_queue = Arc::clone(&queue);
            device
                .build_output_stream(
                    &config.config(),
                    move |out: &mut [f32], _| match cb_queue
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .as_mut()
                    {
                        Some(q) => q.fill(out),
                        None => out.fill(0.0),
                    },
                    |e| peerdesk_net::note!(warn, "声音：播放出错：{e}"),
                    None,
                )
                .map_err(|e| format!("打不开声音输出：{e}"))
        })
    }
}

impl AudioPlayback for SystemPlayback {
    fn play(&self, pcm: &[i16]) {
        let mut stream = self.stream.lock().unwrap_or_else(PoisonError::into_inner);
        if stream.is_none() {
            let opened = self.open();
            if let Err(e) = &opened {
                peerdesk_net::note!(warn, "声音：{e}");
            }
            *stream = Some(opened);
        }
        if let Some(q) = self
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            q.push(pcm);
        }
    }
}

/// 控制端的声音去向。
#[must_use]
pub fn system_playback() -> PlaybackHandle {
    PlaybackHandle(Arc::new(SystemPlayback {
        queue: Arc::new(Mutex::new(None)),
        stream: Mutex::new(None),
    }))
}

/// 被控端：本机正在播放的声音。
#[derive(Debug, Default)]
pub struct SystemCapture;

#[cfg(windows)]
fn capture_device() -> Result<cpal::Device, String> {
    // 在输出设备上开输入流，cpal 用 WASAPI 回环模式录下正在播放的声音
    cpal::default_host()
        .default_output_device()
        .ok_or_else(|| "这台电脑没有声音输出设备，没有可远程的声音".to_owned())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn capture_device() -> Result<cpal::Device, String> {
    // ALSA 的 pulse 插件录“默认输出的监听源”，也就是正在播放的声音（PipeWire 同样支持）。
    // 只影响本进程里新建的 PulseAudio 连接；在打开采集之前设置。
    std::env::set_var("PULSE_SOURCE", "@DEFAULT_MONITOR@");
    let host = cpal::default_host();
    host.input_devices()
        .map_err(|e| format!("列不出声音设备：{e}"))?
        .find(|d| d.name().is_ok_and(|n| n == "pulse" || n == "pipewire"))
        .ok_or_else(|| {
            "没有找到 PulseAudio/PipeWire（ALSA 的 pulse 插件），录不到系统声音。\
             请安装 pipewire-alsa 或 pulseaudio 的 ALSA 插件（例如 libasound2-plugins）"
                .to_owned()
        })
}

/// 采集线程：把声卡的采样切成会话用的帧，发到 `tx`。
#[cfg(not(target_os = "macos"))]
fn start_capture(tx: mpsc::Sender<Vec<i16>>) -> Result<StreamThread, String> {
    run_stream("peerdesk-audio-in", move || {
        let device = capture_device()?;
        #[cfg(windows)]
        let config = device.default_output_config();
        #[cfg(not(windows))]
        let config = device.default_input_config();
        let config = config.map_err(|e| format!("读不到声卡格式：{e}"))?;
        let (rate, channels) = (config.sample_rate().0, config.channels());
        peerdesk_net::note!(
            info,
            "声音：采集 {} {rate} Hz {channels} 声道",
            device.name().unwrap_or_default()
        );
        let mut framer = ToFrames::new(rate, channels);
        device
            .build_input_stream(
                &config.config(),
                move |data: &[f32], _| {
                    for frame in framer.push(data) {
                        // 会话处理不过来时丢帧，不阻塞声卡回调
                        let _ = tx.try_send(frame);
                    }
                },
                |e| peerdesk_net::note!(warn, "声音：采集出错：{e}"),
                None,
            )
            .map_err(|e| format!("打不开声音采集：{e}"))
    })
}

/// macOS：系统不允许普通程序直接录系统声音，用 ScreenCaptureKit（macOS 13 起）。
#[cfg(target_os = "macos")]
fn start_capture(tx: mpsc::Sender<Vec<i16>>) -> Result<StreamThread, String> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("peerdesk-audio-in".to_owned())
        .spawn(move || {
            let framer = Mutex::new(ToFrames::new(48_000, 2));
            let grabber = crate::macos::open_audio(Box::new(move |samples| {
                let frames = framer
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(samples);
                for frame in frames {
                    let _ = tx.try_send(frame);
                }
            }));
            match grabber {
                Ok(grabber) => {
                    let _ = ready_tx.send(Ok(()));
                    let _ = stop_rx.recv();
                    drop(grabber);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })
        .map_err(|e| format!("启动声音线程失败：{e}"))?;
    ready_rx
        .recv()
        .map_err(|_| "声音线程意外退出".to_owned())??;
    Ok(StreamThread {
        stop: Some(stop_tx),
    })
}

impl AudioCapture for SystemCapture {
    fn start(&self) -> Result<mpsc::Receiver<Vec<i16>>, String> {
        let (tx, mut rx) = mpsc::channel(32);
        let thread = start_capture(tx)?;
        // 采集线程跟着接收端走：会话丢掉接收端（停止发送）后，转发任务结束，线程句柄随之丢弃，声卡关闭。
        let (out_tx, out_rx) = mpsc::channel::<Vec<i16>>(32);
        tokio::spawn(async move {
            let _thread = thread;
            while let Some(frame) = rx.recv().await {
                if out_tx.send(frame).await.is_err() {
                    break;
                }
            }
        });
        Ok(out_rx)
    }
}

/// 被控端的声音来源。
#[must_use]
pub fn system_capture() -> CaptureHandle {
    CaptureHandle(Arc::new(SystemCapture))
}
