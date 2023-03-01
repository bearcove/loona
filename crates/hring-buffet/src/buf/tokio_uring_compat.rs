unsafe impl<T> tokio_uring::buf::IoBuf for T
where
    T: crate::IoBuf,
{
    // TODO: forward methods
}

unsafe impl<T> tokio_uring::buf::IoBufMut for T
where
    T: crate::IoBufMut,
{
    // TODO: forward methods
}
