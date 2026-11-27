//! 操作时间线：一次会话里被控端每一拍的画面，写成**内容描述**（没有像素）。
//!
//! 全模型仿真（L0，见 `docs/research/Q-redesign/a1-plan.md`）重放它，不再生成像素。现在只有一种来源：把像素版的
//! 合成桌面（[`RealisticDesktop`]）按同样的阶段跑一遍，每变一版就用产品里的算法（`peerdesk_media::content::describe`）
//! 取一份描述。这样同一个场景的 L0.5（真像素、假后端）和 L0（内容描述、假后端）是同一份内容，交给假后端的数逐帧相同，
//! 对账才有意义。以后会话记录回放、操作模型生成的时间线也是这个类型。
//!
//! 导出要画一遍像素，比重放慢得多，所以按（尺寸、种子、阶段）缓存最近几条：同一个画面配不同的网络、种子、算法反复跑时
//! 只导出一次。

use std::sync::{Arc, Mutex, PoisonError};

use peerdesk_core::frame::ContentDesc;
use peerdesk_core::scale::{FULL_PERMILLE, START_PERMILLE};
use peerdesk_core::Error;

use crate::content::RealisticDesktop;
use crate::Phase;

/// 编码时可能用到的比例：原尺寸，和降分辨率时的半尺寸（见 `peerdesk_core::scale`）。
const SCALES: [u16; 2] = [FULL_PERMILLE, START_PERMILLE];
/// 缓存最近几条时间线（一条 720p、一分钟的时间线约一两百 MB，不能无限留）。
const CACHED: usize = 3;

/// 一拍：画面变没变，这一拍结束时画面的描述。
#[derive(Debug, Clone)]
struct Tick {
    moved: bool,
    content: Arc<ContentDesc>,
}

/// 一次会话的操作时间线。
#[derive(Debug)]
pub struct Timeline {
    ticks: Vec<Tick>,
}

type Key = (u16, u16, u64, Vec<Phase>);

static CACHE: Mutex<Vec<(Key, Arc<Timeline>)>> = Mutex::new(Vec::new());

impl Timeline {
    /// 把像素版的合成桌面按 `phases` 跑一遍，导出时间线（不走缓存）。
    ///
    /// # Errors
    /// 尺寸不合法。
    pub fn extract(width: u16, height: u16, seed: u64, phases: &[Phase]) -> Result<Self, Error> {
        let mut desk = RealisticDesktop::new(width, height, seed)?;
        let mut version = 0u64;
        let mut ticks: Vec<Tick> = Vec::new();
        for phase in phases {
            for _ in 0..phase.frames {
                let moved = desk.step(phase.action)?;
                if moved {
                    version = version.saturating_add(1);
                }
                // 画面没变就沿用上一拍的描述（同一份，不拷贝）
                let content = match ticks.last() {
                    Some(last) if !moved => Arc::clone(&last.content),
                    Some(_) | None => Arc::new(peerdesk_media::content::describe(
                        desk.screen(),
                        version,
                        &SCALES,
                    )?),
                };
                ticks.push(Tick { moved, content });
            }
        }
        Ok(Self { ticks })
    }

    /// 同 [`Self::extract`]，先看缓存。
    ///
    /// # Errors
    /// 同 [`Self::extract`]。
    pub fn cached(
        width: u16,
        height: u16,
        seed: u64,
        phases: &[Phase],
    ) -> Result<Arc<Self>, Error> {
        let key: Key = (width, height, seed, phases.to_vec());
        let hit = CACHE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, t)| Arc::clone(t));
        if let Some(found) = hit {
            return Ok(found);
        }
        // 不拿着锁导出：并行的测试各导各的，偶尔重复导一次也无妨
        let made = Arc::new(Self::extract(width, height, seed, phases)?);
        let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        if cache.len() >= CACHED {
            cache.remove(0);
        }
        cache.push((key, Arc::clone(&made)));
        Ok(made)
    }

    /// 一共多少拍。
    #[must_use]
    pub fn len(&self) -> usize {
        self.ticks.len()
    }

    /// 有没有拍。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ticks.is_empty()
    }

    /// 第 `index` 拍：画面变没变，这一拍结束时的描述。超出范围为 `None`。
    #[must_use]
    pub fn tick(&self, index: usize) -> Option<(bool, &Arc<ContentDesc>)> {
        self.ticks.get(index).map(|t| (t.moved, &t.content))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::Action;

    fn phases() -> Vec<Phase> {
        vec![
            Phase {
                action: Action::Idle,
                frames: 3,
            },
            Phase {
                action: Action::Typing,
                frames: 9,
            },
        ]
    }

    /// 时间线和像素桌面一拍对一拍：变没变相同，版本只在变的时候加一，没变的拍沿用同一份描述；
    /// 每份描述都带原尺寸和半尺寸两种亮度抽样。
    #[test]
    fn the_timeline_follows_the_pixel_desktop_tick_by_tick() {
        let t = Timeline::extract(320, 240, 5, &phases()).unwrap();
        assert_eq!(t.len(), 12);
        assert!(!t.is_empty());
        let mut desk = RealisticDesktop::new(320, 240, 5).unwrap();
        let mut version = 0;
        let mut prev: Option<Arc<ContentDesc>> = None;
        let actions: Vec<Action> = phases()
            .iter()
            .flat_map(|p| (0..p.frames).map(|_| p.action))
            .collect();
        for (i, action) in actions.into_iter().enumerate() {
            let moved = desk.step(action).unwrap();
            version += u64::from(moved);
            let (m, content) = t.tick(i).unwrap();
            assert_eq!(m, moved, "第 {i} 拍");
            assert_eq!(content.version, version);
            assert_eq!(content.size, (320, 240));
            assert!(content.luma_at((320, 240)).is_some() && content.luma_at((160, 120)).is_some());
            assert!(content.luma_at((2, 2)).is_none());
            if let Some(p) = prev.as_ref().filter(|_| !moved) {
                assert!(Arc::ptr_eq(p, content), "没变的拍沿用同一份");
            }
            prev = Some(Arc::clone(content));
        }
        assert!(version > 0, "打字的阶段画面要变");
        assert!(t.tick(12).is_none());
    }

    /// 缓存：同样的参数拿到同一条；留的条数有上限。
    #[test]
    fn timelines_are_cached_by_what_defines_them() {
        let a = Timeline::cached(320, 240, 901, &phases()).unwrap();
        let b = Timeline::cached(320, 240, 901, &phases()).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        for seed in 902..902 + u64::try_from(CACHED).unwrap() + 1 {
            Timeline::cached(320, 240, seed, &phases()).unwrap();
        }
        assert!(CACHE.lock().unwrap().len() <= CACHED);
        assert!(Timeline::extract(0, 0, 1, &phases()).is_err());
    }
}
