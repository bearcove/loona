use bytemuck::TransparentWrapper;

#[repr(transparent)]
pub struct BufCompat<T>(pub T);

unsafe impl<T> TransparentWrapper<T> for BufCompat<T> {}

unsafe impl<T> tokio_uring::buf::IoBuf for BufCompat<T>
where
    T: crate::buf::IoBuf,
{
    #[inline(always)]
    fn stable_ptr(&self) -> *const u8 {
        self.0.stable_ptr()
    }

    #[inline(always)]
    fn bytes_init(&self) -> usize {
        self.0.bytes_init()
    }

    #[inline(always)]
    fn bytes_total(&self) -> usize {
        self.0.bytes_total()
    }
}

unsafe impl<T> tokio_uring::buf::IoBufMut for BufCompat<T>
where
    T: crate::buf::IoBufMut,
{
    #[inline(always)]
    fn stable_mut_ptr(&mut self) -> *mut u8 {
        self.0.stable_mut_ptr()
    }

    #[inline(always)]
    unsafe fn set_init(&mut self, pos: usize) {
        self.0.set_init(pos)
    }
}
