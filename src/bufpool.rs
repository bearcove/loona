use std::{
    cell::{RefCell, RefMut},
    collections::VecDeque,
    marker::PhantomData,
    ops,
};

use memmap2::MmapMut;

#[thread_local]
static BUF_POOL: BufPool = BufPool::new_empty(4096, 4096);

thread_local! {
    static BUF_POOL_DESTRUCTOR: RefCell<Option<MmapMut>> = RefCell::new(None);
}

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("could not mmap buffer")]
    Mmap(#[from] std::io::Error),

    #[error("out of memory")]
    OutOfMemory,
}

/// A buffer pool
pub(crate) struct BufPool {
    buf_size: usize,
    num_buf: usize,
    inner: RefCell<Option<BufPoolInner>>,
}

struct BufPoolInner {
    // this is tied to an [MmapMut] that gets deallocated at thread exit
    // thanks to [BUF_POOL_DESTRUCTOR]
    ptr: *mut u8,
    free: VecDeque<u16>,
}

impl BufPool {
    pub(crate) const fn new_empty(buf_size: u16, num_buf: u16) -> BufPool {
        BufPool {
            buf_size: buf_size as usize,
            num_buf: num_buf as usize,
            inner: RefCell::new(None),
        }
    }

    pub(crate) fn alloc(&self) -> Result<Buf> {
        let mut inner = self.borrow_mut()?;

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
        let inner = inner.as_mut().unwrap();
        inner.free.push_back(index);
    }

    #[cfg(test)]
    pub(crate) fn num_free(&self) -> Result<usize> {
        Ok(self.borrow_mut()?.free.len())
    }

    fn borrow_mut(&self) -> Result<RefMut<BufPoolInner>> {
        let mut inner = self.inner.borrow_mut();
        if inner.is_none() {
            let len = self.num_buf as usize * self.buf_size as usize;
            let mut map = memmap2::MmapOptions::new().len(len).map_anon()?;
            let ptr = map.as_mut_ptr();

            let mut free = VecDeque::with_capacity(self.num_buf as usize);
            for i in 0..self.num_buf {
                free.push_back(i as u16);
            }

            BUF_POOL_DESTRUCTOR.with(|destructor| {
                *destructor.borrow_mut() = Some(map);
            });

            *inner = Some(BufPoolInner { ptr, free });
        }

        let r = RefMut::map(inner, |o| o.as_mut().unwrap());
        Ok(r)
    }

    /// Returns the base pointer for a block
    ///
    /// # Safety
    ///
    /// Borrow-checking is on you!
    #[inline(always)]
    unsafe fn base_ptr(&self, index: u16) -> *mut u8 {
        let start = index as usize * BUF_POOL.buf_size;
        let ptr = BUF_POOL.inner.borrow_mut().as_mut().unwrap().ptr;
        ptr.add(start)
    }
}

pub fn init() -> Result<()> {
    BUF_POOL.borrow_mut()?;
    Ok(())
}

pub struct Buf {
    index: u16,
    // makes this type non-Send
    _non_send: PhantomData<*mut ()>,
}

impl Buf {
    #[inline(always)]
    pub fn alloc() -> Result<Buf, Error> {
        BUF_POOL.alloc()
    }

    #[inline(always)]
    pub fn len() -> usize {
        BUF_POOL.buf_size
    }
}

impl ops::Deref for Buf {
    type Target = [u8];

    #[inline(always)]
    fn deref(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(BUF_POOL.base_ptr(self.index), BUF_POOL.buf_size) }
    }
}

impl ops::DerefMut for Buf {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { std::slice::from_raw_parts_mut(BUF_POOL.base_ptr(self.index), BUF_POOL.buf_size) }
    }
}

unsafe impl tokio_uring::buf::IoBuf for Buf {
    fn stable_ptr(&self) -> *const u8 {
        unsafe { BUF_POOL.base_ptr(self.index) as *const u8 }
    }

    fn bytes_init(&self) -> usize {
        // no-op: buffers are zero-initialized, and users should be careful
        // not to read bonus data
        BUF_POOL.buf_size
    }

    fn bytes_total(&self) -> usize {
        BUF_POOL.buf_size
    }
}

unsafe impl tokio_uring::buf::IoBufMut for Buf {
    fn stable_mut_ptr(&mut self) -> *mut u8 {
        unsafe { BUF_POOL.base_ptr(self.index) }
    }

    unsafe fn set_init(&mut self, _pos: usize) {
        // no-op: buffers are zero-initialized, and users should be careful
        // not to read bonus data
    }
}

impl Drop for Buf {
    fn drop(&mut self) {
        BUF_POOL.reclaim(self.index);
    }
}

#[cfg(test)]
mod tests {
    use crate::bufpool::BUF_POOL;

    use super::Buf;

    #[test]
    fn simple_bufpool_test() -> eyre::Result<()> {
        let initial_free = BUF_POOL.num_free()?;

        let mut buf = Buf::alloc().unwrap();

        let now_free = BUF_POOL.num_free()?;
        assert_eq!(initial_free - 1, now_free);
        assert_eq!(buf.len(), 4096);

        buf[..11].copy_from_slice(b"hello world");
        assert_eq!(&buf[..11], b"hello world");

        drop(buf);

        let now_free = BUF_POOL.num_free()?;
        assert_eq!(initial_free, now_free);

        Ok(())
    }
}
