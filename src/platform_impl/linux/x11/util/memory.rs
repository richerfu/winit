use std::ops::{Deref, DerefMut};

use super::*;

pub(crate) struct XSmartPointer<'a, T> {
    xconn: &'a XConnection,
    pub ptr: *mut T,
}

impl<'a, T> XSmartPointer<'a, T> {
    // You're responsible for only passing things to this that should be XFree'd.
    // Returns None if ptr is null.
    pub fn new(xconn: &'a XConnection, ptr: *mut T) -> Option<Self> {
        if !ptr.is_null() {
            Some(XSmartPointer { xconn, ptr })
        } else {
            None
        }
    }
}

impl<T> Deref for XSmartPointer<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.ptr }
    }
}

impl<T> DerefMut for XSmartPointer<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.ptr }
    }
}

impl<T> Drop for XSmartPointer<'_, T> {
    fn drop(&mut self) {
        unsafe {
            (self.xconn.xlib.XFree)(self.ptr as *mut _);
        }
    }
}
