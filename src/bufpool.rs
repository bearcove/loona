use std::{cell::RefCell, collections::VecDeque, marker::PhantomData, ops, rc::Rc};

use memmap2::MmapMut;

thread_local! {
    static BUF_POOL: Rc<BufPool> = Rc::new(BufPool::new(4096, 1024).unwrap());
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("could not mmap buffer")]
    Mmap(#[from] std::io::Error),

    #[error("out of memory")]
    OutOfMemory,
}

/// A buffer pool
pub(crate) struct BufPool {
    map: MmapMut,
    ptr: *mut u8,
    buf_size: usize,
    num_buf: usize,
    inner: RefCell<BufPoolInner>,
}

struct BufPoolInner {
    free: VecDeque<u16>,
}

impl BufPool {
    pub(crate) fn new(buf_size: u16, num_buf: u16) -> Result<BufPool, Error> {
        let len = num_buf as usize * buf_size as usize;
        let mut map = memmap2::MmapOptions::new().len(len).map_anon()?;
        let ptr = map.as_mut_ptr();

        let mut free = VecDeque::with_capacity(num_buf as usize);
        for i in 0..num_buf {
            free.push_back(i);
        }

        let inner = BufPoolInner { free };
        Ok(BufPool {
            map,
            ptr,
            buf_size: buf_size as usize,
            num_buf: num_buf as usize,
            inner: RefCell::new(inner),
        })
    }

    pub(crate) fn alloc(self: &Rc<Self>) -> Result<Buf, Error> {
        let mut inner = self.inner.borrow_mut();
        if let Some(index) = inner.free.pop_front() {
            Ok(Buf {
                index,
                _non_send: Default::default(),
            })
        } else {
            Err(Error::OutOfMemory)
        }
    }

    fn reclaim(&self, index: u16) {
        let mut inner = self.inner.borrow_mut();
        inner.free.push_back(index);
    }

    fn num_free(&self) -> usize {
        self.inner.borrow().free.len()
    }
}

pub struct Buf {
    index: u16,
    // makes this type non-Send
    _non_send: PhantomData<*mut ()>,
}

impl Buf {
    pub fn alloc() -> Result<Buf, Error> {
        BUF_POOL.with(|pool| pool.alloc())
    }

    pub fn len() -> usize {
        BUF_POOL.with(|pool| pool.buf_size)
    }
}

impl ops::Deref for Buf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        BUF_POOL.with(|pool| {
            let start = self.index as usize * pool.buf_size;
            // TODO: review safety of this. the thread can end, which would run
            // the destructor, drop the `MmapMut`, and unmap the memory. but
            // `Buf` is !Send and !Sync, and the returned slice has the same
            // lifetime as `Buf`, so I think this is okay? - amos
            unsafe { std::slice::from_raw_parts(pool.ptr.add(start), pool.buf_size) }
        })
    }
}

impl ops::DerefMut for Buf {
    fn deref_mut(&mut self) -> &mut Self::Target {
        BUF_POOL.with(|pool| {
            let start = self.index as usize * pool.buf_size;
            unsafe { std::slice::from_raw_parts_mut(pool.ptr.add(start), pool.buf_size) }
        })
    }
}

impl Drop for Buf {
    fn drop(&mut self) {
        BUF_POOL.with(|pool| pool.reclaim(self.index));
    }
}

#[cfg(test)]
mod tests {
    use crate::bufpool::BUF_POOL;

    use super::Buf;

    #[test]
    fn simple_bufpool_test() {
        let initial_free = BUF_POOL.with(|pool| pool.num_free());

        let mut buf = Buf::alloc().unwrap();

        let now_free = BUF_POOL.with(|pool| pool.num_free());
        assert_eq!(initial_free - 1, now_free);
        assert_eq!(buf.len(), 4096);

        buf[..11].copy_from_slice(b"hello world");
        assert_eq!(&buf[..11], b"hello world");

        drop(buf);

        let now_free = BUF_POOL.with(|pool| pool.num_free());
        assert_eq!(initial_free, now_free);
    }
}
