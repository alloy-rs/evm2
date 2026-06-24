use core::any::TypeId;

/// Provides lifetime-erased type identity and unchecked downcasting for erased EVM objects.
///
/// Warning: lifetime parameters are erased before comparing type IDs or downcasting.
pub trait NonStaticAny {
    /// Returns the lifetime-erased type ID of `self`.
    #[inline]
    fn type_id(&self) -> TypeId {
        typeid::of::<Self>()
    }
}

impl<T: ?Sized> NonStaticAny for T {}

impl dyn NonStaticAny + '_ {
    /// Returns whether this object has type `T`, ignoring lifetime parameters.
    #[inline]
    pub fn is<T: NonStaticAny>(&self) -> bool {
        self.type_id() == typeid::of::<T>()
    }

    /// Downcasts this object to a concrete type.
    #[inline]
    pub fn downcast_ref<T: NonStaticAny>(&self) -> Option<&T> {
        self.is::<T>().then(|| unsafe { &*(self as *const Self).cast::<T>() })
    }

    /// Mutably downcasts this object to a concrete type.
    #[inline]
    pub fn downcast_mut<T: NonStaticAny>(&mut self) -> Option<&mut T> {
        self.is::<T>().then(|| unsafe { &mut *(self as *mut Self).cast::<T>() })
    }
}
