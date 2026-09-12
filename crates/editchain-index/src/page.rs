//! Borrowed page access with a typed failure boundary around native requests.

use crate::storage::{active, with_storage, Address, Storage};
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize, Serializer};
use std::{
    cell::{OnceCell, RefCell},
    io,
    ops::{Deref, DerefMut},
    rc::Rc,
};

#[derive(Debug)]
struct Fault(io::Error);

/// Abort one native transaction on a lazy page IO/decoding failure. Other
/// panics retain their original behavior and are never converted to missing data.
///
/// # Errors
/// Returns the original index error from a failed page access.
pub fn boundary<T>(work: impl FnOnce() -> T) -> io::Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
        Ok(value) => Ok(value),
        Err(payload) => match payload.downcast::<Fault>() {
            Ok(fault) => Err(fault.0),
            Err(payload) => std::panic::resume_unwind(payload),
        },
    }
}

#[expect(
    clippy::panic,
    reason = "Borrowed lazy page access aborts its transaction; boundary converts this private IO fault back to Result"
)]
fn fault(error: io::Error) -> ! {
    std::panic::panic_any(Fault(error))
}

pub(crate) fn fail(error: io::Error) -> ! {
    fault(error)
}

type Loader<T> = fn(&Rc<Storage>, Address) -> io::Result<T>;

/// A lazily decoded immutable page until first mutable access. Page failures
/// abort the enclosing [`boundary`]; they never act like absent keys.
#[derive(Debug)]
pub struct Page<T> {
    cached: OnceCell<Box<T>>,
    address: RefCell<Option<(Rc<Storage>, Address)>>,
    loader: Option<Loader<T>>,
}

impl<T> Page<T> {
    /// Start a dirty page in memory; checkpointing supplies its backing store.
    pub fn new(value: T) -> Self {
        Self {
            cached: OnceCell::from(Box::new(value)),
            address: RefCell::default(),
            loader: None,
        }
    }

    /// Consume a page, hydrating it if necessary.
    pub fn into_inner(mut self) -> T {
        let _loaded = <Self as Deref>::deref(&self);
        *self
            .cached
            .take()
            .unwrap_or_else(|| fault(io::Error::other("empty index page")))
    }

    fn load(&self) -> T {
        let address = self.address.borrow();
        let Some((storage, address)) = address.as_ref() else {
            fault(io::Error::other("index page has neither data nor address"));
        };
        let Some(loader) = self.loader else {
            fault(io::Error::other("index page has no decoder"));
        };
        loader(storage, *address).unwrap_or_else(|error| fault(error))
    }
}

impl<T: Default> Default for Page<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: Clone> Clone for Page<T> {
    fn clone(&self) -> Self {
        // Sharing immutable disk addresses does not copy or hydrate old state.
        if self.loader.is_some() && self.address.borrow().is_some() {
            Self {
                cached: OnceCell::new(),
                address: self.address.clone(),
                loader: self.loader,
            }
        } else {
            Self::new(self.deref().clone())
        }
    }
}

impl<T> Deref for Page<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.cached.get_or_init(|| Box::new(self.load()))
    }
}

impl<T> DerefMut for Page<T> {
    fn deref_mut(&mut self) -> &mut T {
        let _loaded = <Self as Deref>::deref(self);
        drop(self.address.get_mut().take());
        self.cached
            .get_mut()
            .unwrap_or_else(|| fault(io::Error::other("empty mutable index page")))
    }
}

impl<T: Serialize> Serialize for Page<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let storage = active().map_err(serde::ser::Error::custom)?;
        if let Some((source, address)) = self.address.borrow().as_ref() {
            if Rc::ptr_eq(&storage, source) {
                return address.serialize(serializer);
            }
        }
        let address = storage.write(&**self).map_err(serde::ser::Error::custom)?;
        *self.address.borrow_mut() = Some((storage, address));
        address.serialize(serializer)
    }
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for Page<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let address = Address::deserialize(deserializer)?;
        let storage = active().map_err(serde::de::Error::custom)?;
        Ok(Self {
            cached: OnceCell::new(),
            address: RefCell::new(Some((storage, address))),
            loader: Some(|storage, address| with_storage(storage, || storage.read(address))),
        })
    }
}
