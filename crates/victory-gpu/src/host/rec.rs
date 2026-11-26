//! A host list's D3D12 list, with barrier calls joined
//! (`VICTORY_JOIN_BARRIERS=1`; off by default; A/B-able with
//! `VICTORY_AB=<s>,VICTORY_JOIN_BARRIERS`).
//!
//! The state changes the host asks for (`crate::list::ensure_state`, the
//! aliasing before a texture's first upload) and the title's own barriers
//! wait here instead of going to the list at once; they go, in the order
//! they were asked for, in one call, at the next use of the list for
//! anything else. Every use goes through [`Rec`]'s `Deref`, so nothing is
//! recorded before a barrier asked for earlier. In a replay of capture
//! replay5 joining the calls that had no work between them saved 2.4 ms of
//! 79.5 (2,490 calls joined, 1,619 of them a texture's state change after
//! its upload followed by the next texture's aliasing).
//!
//! While they are held, a state change that carries on from a held one of
//! the same resource is folded into it ([`join`]): A to B then B to C is
//! held as A to C, and A to B then B to A goes (or becomes an unordered
//! access barrier on the resource, if A is unordered access); a barrier on
//! every resource right after another is dropped. That covers two changes
//! of one texture in one title call too, which D3D12 does not allow in one
//! call. The rules are upstream's barrier queue (Marvisak/VIctory#1382).
//! Not measured.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use windows::Win32::Graphics::Direct3D12::*;

/// Whether barriers are held now (`VICTORY_JOIN_BARRIERS=1`, or its phase
/// of `VICTORY_AB`).
pub(crate) fn on() -> bool {
    #[cfg(feature = "diagnostics")]
    if let Some(on) = crate::ab::pick("VICTORY_JOIN_BARRIERS") {
        return on;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_JOIN_BARRIERS").is_some_and(|v| v == "1"))
}

/// Barriers held. Their resources are borrowed: the list's states hold the
/// textures (`HostList::transition`), and the title's barriers name what
/// the title keeps alive until the list has run.
struct Held(Vec<D3D12_RESOURCE_BARRIER>);

// SAFETY: borrowed interface pointers of free-threaded objects.
unsafe impl Send for Held {}

/// Since the last frame-rate line: barrier calls asked for while held,
/// calls made of them, and barriers folded away ([`join`]).
pub(crate) static STATS: [std::sync::atomic::AtomicU64; 3] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 3];

pub(crate) struct Rec {
    list: ID3D12GraphicsCommandList,
    any: AtomicBool,
    held: Mutex<Held>,
}

impl Rec {
    pub(crate) fn new(list: ID3D12GraphicsCommandList) -> Self {
        Self { list, any: AtomicBool::new(false), held: Mutex::new(Held(Vec::new())) }
    }

    /// Records `barriers` now, or holds them for the next use of the list.
    ///
    /// # Safety
    /// As `ResourceBarrier`'s: an open list, live resources.
    pub(crate) unsafe fn barriers(&self, barriers: &[D3D12_RESOURCE_BARRIER]) {
        if barriers.is_empty() {
            return;
        }
        if !on() {
            // (What was held while it was on goes first.)
            if self.any.load(Relaxed) {
                self.flush();
            }
            // SAFETY: the caller's.
            unsafe { self.list.ResourceBarrier(barriers) };
            return;
        }
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let mut folded = 0;
        for barrier in barriers {
            match join(&mut held.0, barrier) {
                Joined::Folded => folded += 1,
                Joined::Added => {}
                // A resource whose state changes again in a way that does
                // not fold: what is held goes first, so that no call holds
                // two changes of one resource.
                Joined::Conflict => {
                    // SAFETY: as in `flush`.
                    unsafe { self.list.ResourceBarrier(&held.0) };
                    held.0.clear();
                    STATS[1].fetch_add(1, Relaxed);
                    // SAFETY: as in `join`.
                    held.0.push(unsafe { std::ptr::read(barrier) });
                }
            }
        }
        STATS[0].fetch_add(1, Relaxed);
        STATS[2].fetch_add(folded, Relaxed);
        self.any.store(true, Relaxed);
    }

    /// What is held goes to the list, in one call.
    fn flush(&self) {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        self.any.store(false, Relaxed);
        if held.0.is_empty() {
            return;
        }
        // SAFETY: the barriers were asked for on this open list, in order;
        // their resources are alive (`Held`).
        unsafe { self.list.ResourceBarrier(&held.0) };
        held.0.clear();
        STATS[1].fetch_add(1, Relaxed);
    }
}

/// What [`join`] did with a barrier.
#[derive(Debug, PartialEq, Eq)]
enum Joined {
    /// Folded into a held one, or dropped: nothing more to record.
    Folded,
    /// Held after the others.
    Added,
    /// Not held: it names a held resource and does not fold. What is held
    /// has to go to the list first.
    Conflict,
}

/// Adds `barrier` to the `held` ones, folding a state change into the held
/// one of the same resource it carries on from. Only held state changes
/// are looked at, from the last back, up to the first barrier of another
/// kind (past one of those the order counts). The held one folds if it is
/// of the same subresource, neither is split (begin or end only) and it
/// ends where the new one starts: A to B then B to C makes A to C; A to B
/// then B to A makes none, or an unordered access barrier on the resource
/// when A is unordered access (shader writes before still finish before
/// shader access after). A barrier on every resource right after another
/// is dropped. Anything else naming a held resource is a conflict, and
/// `held` is left as it was.
fn join(held: &mut Vec<D3D12_RESOURCE_BARRIER>, barrier: &D3D12_RESOURCE_BARRIER) -> Joined {
    let resource = named(barrier);
    if barrier.Type == D3D12_RESOURCE_BARRIER_TYPE_TRANSITION
        && barrier.Flags == D3D12_RESOURCE_BARRIER_FLAG_NONE
        && resource.is_some()
    {
        // SAFETY: the union's member is the one `Type` says.
        let new = unsafe { &barrier.Anonymous.Transition };
        for i in (0..held.len()).rev() {
            if held[i].Type != D3D12_RESOURCE_BARRIER_TYPE_TRANSITION {
                break;
            }
            if named(&held[i]) != resource {
                continue;
            }
            let flags = held[i].Flags;
            // SAFETY: as above.
            let prev = unsafe { &mut held[i].Anonymous.Transition };
            if flags != D3D12_RESOURCE_BARRIER_FLAG_NONE
                || prev.Subresource != new.Subresource
                || prev.StateAfter != new.StateBefore
            {
                break;
            }
            if prev.StateBefore != new.StateAfter {
                prev.StateAfter = new.StateAfter;
            } else if new.StateAfter.0 & D3D12_RESOURCE_STATE_UNORDERED_ACCESS.0 != 0 {
                // SAFETY: a copy of a borrowed pointer, released through
                // neither barrier (`ManuallyDrop`).
                let resource = unsafe { std::ptr::read(&prev.pResource) };
                held[i] = D3D12_RESOURCE_BARRIER {
                    Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
                    Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
                    Anonymous: D3D12_RESOURCE_BARRIER_0 {
                        UAV: std::mem::ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER {
                            pResource: resource,
                        }),
                    },
                };
            } else {
                held.remove(i);
            }
            return Joined::Folded;
        }
    } else if global_uav(barrier) && held.last().is_some_and(global_uav) {
        return Joined::Folded;
    }
    if resource.is_some_and(|r| held.iter().filter_map(named).any(|h| h == r)) {
        return Joined::Conflict;
    }
    // SAFETY: a plain copy of the description; nothing is released through
    // it (its pointers are borrowed, `ManuallyDrop`).
    held.push(unsafe { std::ptr::read(barrier) });
    Joined::Added
}

/// Whether `b` is an unordered access barrier on every resource.
fn global_uav(b: &D3D12_RESOURCE_BARRIER) -> bool {
    // SAFETY: the union's member is the one `Type` says; a pointer read.
    b.Type == D3D12_RESOURCE_BARRIER_TYPE_UAV && unsafe { b.Anonymous.UAV.pResource.is_none() }
}

/// The resource a state change or aliasing names (its interface pointer).
fn named(b: &D3D12_RESOURCE_BARRIER) -> Option<usize> {
    // SAFETY: the union's member is the one `Type` says; a pointer read.
    unsafe {
        let p: Option<&ID3D12Resource> = match b.Type {
            D3D12_RESOURCE_BARRIER_TYPE_TRANSITION => b.Anonymous.Transition.pResource.as_ref(),
            D3D12_RESOURCE_BARRIER_TYPE_ALIASING => b.Anonymous.Aliasing.pResourceAfter.as_ref(),
            _ => None,
        };
        p.map(|r| windows::core::Interface::as_raw(r) as usize)
    }
}

impl std::ops::Deref for Rec {
    type Target = ID3D12GraphicsCommandList;

    /// The list, with what is held recorded first.
    fn deref(&self) -> &ID3D12GraphicsCommandList {
        if self.any.load(Relaxed) {
            self.flush();
        }
        &self.list
    }
}

/// For the frame-rate line.
pub(crate) fn line(frames: f64) {
    let [asked, made, folded] = [0, 1, 2].map(|i| STATS[i].swap(0, Relaxed) as f64 / frames);
    if asked > 0.0 {
        tracing::info!(
            "barrier calls per frame (VICTORY_JOIN_BARRIERS): {asked:.0} asked for, {made:.0} made of them; {folded:.0} barriers folded away"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::ManuallyDrop;

    const A: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_COPY_DEST;
    const B: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_COPY_SOURCE;
    const C: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE;
    const U: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_UNORDERED_ACCESS;

    /// A made-up resource pointer: only compared, never called or released.
    fn resource(n: usize) -> ManuallyDrop<Option<ID3D12Resource>> {
        // SAFETY: a non-null pointer-sized value; `ManuallyDrop` keeps it
        // from being released.
        ManuallyDrop::new(unsafe {
            std::mem::transmute::<usize, Option<ID3D12Resource>>(n * 0x1000)
        })
    }

    fn change(
        n: usize,
        before: D3D12_RESOURCE_STATES,
        after: D3D12_RESOURCE_STATES,
    ) -> D3D12_RESOURCE_BARRIER {
        D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: resource(n),
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: before,
                    StateAfter: after,
                }),
            },
        }
    }

    fn uav(n: Option<usize>) -> D3D12_RESOURCE_BARRIER {
        D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                UAV: ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER {
                    pResource: n.map_or(ManuallyDrop::new(None), resource),
                }),
            },
        }
    }

    /// A barrier as (kind, resource, before, after); a UAV barrier's states
    /// are zero.
    fn seen(b: &D3D12_RESOURCE_BARRIER) -> (i32, usize, i32, i32) {
        // SAFETY: the union's member is the one `Type` says.
        unsafe {
            match b.Type {
                D3D12_RESOURCE_BARRIER_TYPE_TRANSITION => {
                    let t = &b.Anonymous.Transition;
                    (
                        b.Type.0,
                        named(b).unwrap_or(0),
                        t.StateBefore.0,
                        t.StateAfter.0,
                    )
                }
                _ => {
                    let p = b.Anonymous.UAV.pResource.as_ref();
                    (
                        b.Type.0,
                        p.map_or(0, |r| windows::core::Interface::as_raw(r) as usize),
                        0,
                        0,
                    )
                }
            }
        }
    }

    fn joined(barriers: &[D3D12_RESOURCE_BARRIER]) -> (Vec<Joined>, Vec<(i32, usize, i32, i32)>) {
        let mut held = Vec::new();
        let done = barriers.iter().map(|b| join(&mut held, b)).collect();
        (done, held.iter().map(seen).collect())
    }

    #[test]
    fn a_change_that_carries_on_folds() {
        let (done, held) = joined(&[change(1, A, B), change(2, A, B), change(1, B, C)]);
        assert_eq!(done, [Joined::Added, Joined::Added, Joined::Folded]);
        assert_eq!(held, [seen(&change(1, A, C)), seen(&change(2, A, B))]);
    }

    #[test]
    fn a_change_back_goes() {
        let (done, held) = joined(&[change(1, A, B), change(1, B, A)]);
        assert_eq!(done, [Joined::Added, Joined::Folded]);
        assert!(held.is_empty());
    }

    #[test]
    fn a_change_back_to_unordered_access_leaves_a_uav_barrier() {
        let (done, held) = joined(&[change(1, U, B), change(1, B, U)]);
        assert_eq!(done, [Joined::Added, Joined::Folded]);
        assert_eq!(held, [seen(&uav(Some(1)))]);
    }

    #[test]
    fn split_changes_are_left_alone() {
        let mut begin = change(1, A, B);
        begin.Flags = D3D12_RESOURCE_BARRIER_FLAG_BEGIN_ONLY;
        let mut end = change(1, A, B);
        end.Flags = D3D12_RESOURCE_BARRIER_FLAG_END_ONLY;
        let (done, held) = joined(&[begin, change(1, B, C)]);
        assert_eq!(done, [Joined::Added, Joined::Conflict]);
        assert_eq!(held, [seen(&change(1, A, B))]);
        let (done, _) = joined(&[change(1, C, A), end]);
        assert_eq!(done, [Joined::Added, Joined::Conflict]);
    }

    #[test]
    fn a_global_uav_barrier_after_another_goes() {
        let (done, held) = joined(&[uav(None), uav(None), uav(Some(1)), uav(None)]);
        assert_eq!(
            done,
            [Joined::Added, Joined::Folded, Joined::Added, Joined::Added]
        );
        assert_eq!(held.len(), 3);
    }

    #[test]
    fn a_change_that_does_not_carry_on_conflicts() {
        let (done, held) = joined(&[change(1, A, B), change(1, C, A)]);
        assert_eq!(done, [Joined::Added, Joined::Conflict]);
        assert_eq!(held, [seen(&change(1, A, B))]);
        // Nor past a barrier of another kind.
        let (done, _) = joined(&[change(1, A, B), uav(None), change(1, B, C)]);
        assert_eq!(done, [Joined::Added, Joined::Added, Joined::Conflict]);
        // Nor of another subresource.
        let mut one = change(1, B, C);
        // SAFETY: a state change.
        unsafe { (*one.Anonymous.Transition).Subresource = 0 };
        let (done, _) = joined(&[change(1, A, B), one]);
        assert_eq!(done, [Joined::Added, Joined::Conflict]);
    }
}
