use std::{
    cell::{RefCell, RefMut},
    collections::VecDeque,
    marker::PhantomData,
    ops,
};

use memmap2::MmapMut;

#[thread_local]
static BUF_POOL: BufPool = BufPool::new_empty(4096, 1024);

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
    map: MmapMut,
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

    pub(crate) fn num_free(&self) -> Result<usize> {
        Ok(self.borrow_mut()?.free.len())
    }

    fn borrow_mut(&self) -> Result<RefMut<BufPoolInner>> {
        let mut inner = self.inner.borrow_mut();
        if inner.is_none() {
            let len = self.num_buf as usize * self.buf_size as usize;
            let map = memmap2::MmapOptions::new().len(len).map_anon()?;

            let mut free = VecDeque::with_capacity(self.num_buf as usize);
            for i in 0..self.num_buf {
                free.push_back(i as u16);
            }

            *inner = Some(BufPoolInner { map, free });
        }

        let r = RefMut::map(inner, |o| o.as_mut().unwrap());
        Ok(r)
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

    pub fn len() -> usize {
        BUF_POOL.buf_size
    }
}

impl ops::Deref for Buf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        let start = self.index as usize * BUF_POOL.buf_size;
        // TODO: review safety of this. the thread can end, which would run
        // the destructor, drop the `MmapMut`, and unmap the memory. but
        // `Buf` is !Send and !Sync, and the returned slice has the same
        // lifetime as `Buf`, so I think this is okay? - amos
        let ptr = BUF_POOL.inner.borrow().as_ref().unwrap().map.as_ptr();
        unsafe { std::slice::from_raw_parts(ptr.add(start), BUF_POOL.buf_size) }
    }
}

impl ops::DerefMut for Buf {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let start = self.index as usize * BUF_POOL.buf_size;
        let ptr = BUF_POOL
            .inner
            .borrow_mut()
            .as_mut()
            .unwrap()
            .map
            .as_mut_ptr();
        unsafe { std::slice::from_raw_parts_mut(ptr.add(start), BUF_POOL.buf_size) }
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
