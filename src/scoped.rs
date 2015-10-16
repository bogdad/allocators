//! A scoped linear allocator. This is something of a cross between a stack allocator
//! and a traditional linear allocator.

use std::cell::Cell;
use std::mem;
use std::ptr;

use super::{Allocator, AllocatorError, Block, HeapAllocator, HEAP, BlockOwner};

/// A scoped linear allocator.
pub struct Scoped<'parent, A: 'parent + Allocator> {
    allocator: &'parent A,
    current: Cell<*mut u8>,
    end: *mut u8,
    root: bool,
    start: *mut u8,
}

impl Scoped<'static, HeapAllocator> {
    /// Creates a new `Scoped` backed by `size` bytes from the heap.
    pub fn new(size: usize) -> Result<Self, AllocatorError> {
        Scoped::new_from(HEAP, size)
    }
}

impl<'parent, A: Allocator> Scoped<'parent, A> {
    /// Creates a new `Scoped` backed by `size` bytes from the allocator supplied.
    pub fn new_from(alloc: &'parent A, size: usize) -> Result<Self, AllocatorError> {
        // Create a memory buffer with the desired size and maximal align from the parent.
        match unsafe { alloc.allocate_raw(size, mem::align_of::<usize>()) } {
            Ok(block) => Ok(Scoped {
                allocator: alloc,
                current: Cell::new(block.ptr()),
                end: unsafe { block.ptr().offset(block.size() as isize) },
                root: true,
                start: block.ptr(),
            }),
            Err(err) => Err(err),
        }
    }

    /// Calls the supplied function with a new scope of the allocator.
    ///
    /// Returns the result of the closure or an error if this allocator
    /// has already been scoped.
    pub fn scope<F, U>(&self, f: F) -> Result<U, ()>
        where F: FnMut(&Self) -> U
    {
        if self.is_scoped() {
            return Err(())
        }

        let mut f = f;
        let old = self.current.get();
        let alloc = Scoped {
            allocator: self.allocator,
            current: self.current.clone(),
            end: self.end,
            root: false,
            start: old,
        };

        // set the current pointer to null as a flag to indicate
        // that this allocator is being scoped.
        self.current.set(ptr::null_mut());
        let u = f(&alloc);
        self.current.set(old);

        mem::forget(alloc);
        Ok(u)
    }

    // Whether this allocator is currently scoped.
    pub fn is_scoped(&self) -> bool {
        self.current.get().is_null()
    }
}

unsafe impl<'a, A: Allocator> Allocator for Scoped<'a, A> {
    unsafe fn allocate_raw(&self, size: usize, align: usize) -> Result<Block, AllocatorError> {
        if self.is_scoped() {
            return Err(AllocatorError::AllocatorSpecific("Called allocate on already scoped \
                                                          allocator."
                                                             .into()))
        }

        let current_ptr = self.current.get();
        let aligned_ptr = super::align_forward(current_ptr, align);
        let end_ptr = aligned_ptr.offset(size as isize);

        if end_ptr > self.end {
            Err(AllocatorError::OutOfMemory)
        } else {
            self.current.set(end_ptr);
            Ok(Block::new(aligned_ptr, size, align))
        }
    }

    #[allow(unused_variables)]
    unsafe fn deallocate_raw(&self, blk: Block) {
        // no op for this unless this is the last allocation.
        // The memory gets reused when the scope is cleared.
        let current_ptr = self.current.get();
        if !self.is_scoped() && blk.ptr().offset(blk.size() as isize) == current_ptr {
            self.current.set(blk.ptr());
        }
    }
}

impl<'a, A: Allocator> BlockOwner for Scoped<'a, A> {
    fn owns_block(&self, blk: &Block) -> bool {
        let ptr = blk.ptr();

        ptr >= self.start && ptr <= self.end
    }
}

impl<'a, A: Allocator> Drop for Scoped<'a, A> {
    /// Drops the `Scoped`
    fn drop(&mut self) {
        let size = self.end as usize - self.start as usize;
        // only free if this allocator is the root to make sure
        // that memory is freed after destructors for allocated objects
        // are called in case of unwind
        if self.root && size > 0 {
            unsafe { 
                self.allocator.deallocate_raw(
                    Block::new(self.start, size, mem::align_of::<usize>())
                ) 
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    #[should_panic]
    fn use_outer() {
        let alloc = Scoped::new(4).unwrap();
        let mut outer_val = alloc.allocate(0i32).unwrap();
        alloc.scope(|_inner| {
            // using outer allocator is dangerous and should fail.
                 outer_val = alloc.allocate(1i32).unwrap();
             })
             .unwrap();
    }

    #[test]
    fn scope_scope() {
        let alloc = Scoped::new(64).unwrap();
        let _ = alloc.allocate(0).unwrap();
        alloc.scope(|inner| {
                 let _ = inner.allocate(32);
                 inner.scope(|bottom| {
                          let _ = bottom.allocate(23);
                      })
                      .unwrap();
             })
             .unwrap();
    }

    #[test]
    fn out_of_memory() {
        // allocate more memory than the allocator has.
        let alloc = Scoped::new(0).unwrap();
        let (err, _) = alloc.allocate(1i32).err().unwrap();
        assert_eq!(err, AllocatorError::OutOfMemory);
    }

    #[test]
    fn placement_in() {
        let alloc = Scoped::new(8_000_000).unwrap();
        // this would smash the stack otherwise.
        let _big = in alloc.make_place().unwrap() { [0u8; 8_000_000] };
    }

    #[test]
    fn owning() {
        let alloc = Scoped::new(64).unwrap();

        let val = alloc.allocate(1i32).unwrap();
        assert!(alloc.owns(&val));

        alloc.scope(|inner| {
            let in_val = inner.allocate(2i32).unwrap();
            assert!(inner.owns(&in_val));
            assert!(!inner.owns(&val));
        }).unwrap();
    }
}
