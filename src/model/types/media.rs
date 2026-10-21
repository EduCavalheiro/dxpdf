//! The bytes of a media part (a picture), shared by every placement of it.

use std::fmt;
use std::sync::Arc;

/// A media part's bytes: in memory, or read from wherever they live each time
/// they are needed (the painter decodes a picture once per size it is drawn
/// at, then keeps only the decoded bitmap). A document with thousands of
/// photos then never holds them all, only the ones being painted.
///
/// Cloning shares the handle, and [`id`](Self::id) is the handle's identity:
/// what the painter's bitmap cache keys on, as it keyed on the bytes'
/// allocation before.
#[derive(Clone)]
pub struct MediaBytes(Arc<Source>);

enum Source {
    Loaded(Arc<[u8]>),
    Lazy(Arc<dyn Fn() -> Option<Vec<u8>> + Send + Sync>),
}

impl MediaBytes {
    /// Bytes already in memory.
    pub fn from_bytes(bytes: impl Into<Arc<[u8]>>) -> Self {
        MediaBytes(Arc::new(Source::Loaded(bytes.into())))
    }

    /// Bytes `load` reads when they are needed; `None` if they cannot be
    /// read (the picture is then left blank, as an undecodable one is).
    pub fn lazy(load: impl Fn() -> Option<Vec<u8>> + Send + Sync + 'static) -> Self {
        MediaBytes(Arc::new(Source::Lazy(Arc::new(load))))
    }

    /// The bytes: shared when in memory, read now otherwise.
    pub fn bytes(&self) -> Option<Arc<[u8]>> {
        match &*self.0 {
            Source::Loaded(bytes) => Some(Arc::clone(bytes)),
            Source::Lazy(load) => load().map(Arc::from),
        }
    }

    /// The same bytes under a handle of its own: [`id`](Self::id) differs
    /// from this one's. What reading the part out of the package again gives,
    /// so a package whose media are handles paints byte for byte what the same
    /// package with its media in the ZIP does.
    pub fn fresh(&self) -> Self {
        MediaBytes(Arc::new(match &*self.0 {
            Source::Loaded(bytes) => Source::Loaded(Arc::clone(bytes)),
            Source::Lazy(load) => Source::Lazy(Arc::clone(load)),
        }))
    }

    /// This handle's identity, shared by its clones.
    pub fn id(&self) -> *const () {
        Arc::as_ptr(&self.0).cast()
    }
}

impl fmt::Debug for MediaBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &*self.0 {
            Source::Loaded(bytes) => write!(f, "MediaBytes({} bytes)", bytes.len()),
            Source::Lazy(_) => f.write_str("MediaBytes(lazy)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn loaded_bytes_are_shared() {
        let media = MediaBytes::from_bytes(&b"abc"[..]);
        let a = media.bytes().unwrap();
        let b = media.bytes().unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(&*a, b"abc");
    }

    #[test]
    fn lazy_bytes_are_read_each_time_and_not_kept() {
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&reads);
        let media = MediaBytes::lazy(move || {
            counter.fetch_add(1, Ordering::Relaxed);
            Some(b"xyz".to_vec())
        });
        assert_eq!(reads.load(Ordering::Relaxed), 0, "nothing is read up front");
        assert_eq!(&*media.bytes().unwrap(), b"xyz");
        assert_eq!(&*media.bytes().unwrap(), b"xyz");
        assert_eq!(reads.load(Ordering::Relaxed), 2);
        assert!(MediaBytes::lazy(|| None).bytes().is_none());
    }

    #[test]
    fn clones_share_an_identity_and_fresh_handles_do_not() {
        let media = MediaBytes::lazy(|| Some(vec![1]));
        assert_eq!(media.clone().id(), media.id());
        let fresh = media.fresh();
        assert_ne!(fresh.id(), media.id());
        assert_eq!(fresh.bytes().unwrap(), media.bytes().unwrap());
    }
}
