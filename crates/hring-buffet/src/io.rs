use maybe_uring::{
    buf::{IoBuf, IoBufMut},
    BufResult,
};

mod chan;
pub use chan::*;

#[cfg(all(feature = "tokio-uring", target_os = "linux"))]
mod uring_tcp;
#[cfg(all(feature = "tokio-uring", target_os = "linux"))]
pub use uring_tcp::*;

mod non_uring;
pub use non_uring::*;

#[cfg(all(test, not(feature = "miri")))]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use crate::WriteOwned;
    use maybe_uring::{buf::IoBuf, BufResult};

    #[test]
    fn test_write_all() {
        enum Mode {
            WriteZero,
            WritePartial,
        }

        struct Writer {
            mode: Mode,
            bytes: Rc<RefCell<Vec<u8>>>,
        }

        impl WriteOwned for Writer {
            async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
                assert!(buf.bytes_init() > 0, "zero-length writes are forbidden");

                match self.mode {
                    Mode::WriteZero => (Ok(0), buf),
                    Mode::WritePartial => {
                        let n = match buf.bytes_init() {
                            1 => 1,
                            _ => buf.bytes_init() / 2,
                        };
                        let slice = unsafe { std::slice::from_raw_parts(buf.stable_ptr(), n) };
                        self.bytes.borrow_mut().extend_from_slice(slice);
                        (Ok(n), buf)
                    }
                }
            }
        }

        maybe_uring::start(async move {
            let mut writer = Writer {
                mode: Mode::WriteZero,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            let res = writer.write_all(buf_a).await;
            assert!(res.is_err());

            let mut writer = Writer {
                mode: Mode::WriteZero,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            let buf_b = vec![6, 7, 8, 9, 10];
            let res = writer.writev_all(vec![buf_a, buf_b]).await;
            assert!(res.is_err());

            let mut writer = Writer {
                mode: Mode::WritePartial,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            writer.write_all(buf_a).await.unwrap();
            assert_eq!(&writer.bytes.borrow()[..], &[1, 2, 3, 4, 5]);

            let mut writer = Writer {
                mode: Mode::WritePartial,
                bytes: Default::default(),
            };
            let buf_a = vec![1, 2, 3, 4, 5];
            let buf_b = vec![6, 7, 8, 9, 10];
            writer.writev_all(vec![buf_a, buf_b]).await.unwrap();
            assert_eq!(&writer.bytes.borrow()[..], &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        });
    }
}
