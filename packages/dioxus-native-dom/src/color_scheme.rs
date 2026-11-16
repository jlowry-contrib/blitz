//! A handle through which app code sets the document's colour scheme.
use blitz_traits::shell::ColorScheme;
use std::cell::{Cell, RefCell};
use std::fmt;
use std::rc::Rc;
use std::task::Waker;

/// Sets the document's colour scheme from app code, over the one the shell reports.
///
/// A [`DioxusDocument`](crate::DioxusDocument) provides it as a root context. A value
/// set during a render or an effect reaches the frame being drawn; one set in an event
/// handler wakes the document, and reaches the next frame. `None` follows the shell's
/// scheme again.
///
/// It isn't `Send`: nothing would wake the document on a set from another thread.
#[derive(Clone, Default)]
pub struct ColorSchemeOverride {
    inner: Rc<Pending>,
}

#[derive(Default)]
struct Pending {
    scheme: Cell<Option<ColorScheme>>,
    dirty: Cell<bool>,
    /// The waker the document was last polled with, which a set wakes.
    waker: RefCell<Option<Waker>>,
}

impl ColorSchemeOverride {
    /// Sets the scheme the document uses, or `None` to follow the shell's.
    pub fn set(&self, scheme: Option<ColorScheme>) {
        self.inner.scheme.set(scheme);
        self.inner.dirty.set(true);
        if let Some(waker) = &*self.inner.waker.borrow() {
            waker.wake_by_ref();
        }
    }

    /// Returns the scheme last set, or `None` when the document follows the shell.
    pub fn scheme(&self) -> Option<ColorScheme> {
        self.inner.scheme.get()
    }

    /// Returns the value set since the last call, if any.
    pub(crate) fn take_pending(&self) -> Option<Option<ColorScheme>> {
        self.inner
            .dirty
            .replace(false)
            .then(|| self.inner.scheme.get())
    }

    /// Keeps `waker`, the one the document is being polled with, for the next set to wake.
    pub(crate) fn register_waker(&self, waker: &Waker) {
        let mut kept = self.inner.waker.borrow_mut();
        if !kept.as_ref().is_some_and(|kept| kept.will_wake(waker)) {
            *kept = Some(waker.clone());
        }
    }
}

impl fmt::Debug for ColorSchemeOverride {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ColorSchemeOverride")
            .field("scheme", &self.inner.scheme.get())
            .field("dirty", &self.inner.dirty.get())
            .finish_non_exhaustive()
    }
}
