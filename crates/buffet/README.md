# buffet

buffet is designed to memmap a big chunk of memory and then, well, hand out
reference-counted (non-thread-safe) pieces of it. It was faster than stack
allocation in some benchmark I did a while ago.

Also, it's compatible with io_uring in that you can give ownership of one of the
pieces to a read/write operation and nobody else is able to use it until that
operation is done, which is really neat.

There's a bunch of splitting operations that try to maintain reference count
and the general "one mutable reference XOR multiple read-only references" vibe
of the whole endeavor.

I'm not honestly convinced buffet is the optimal way to go about this, but it
works for now, it's io_uring and fallback-codepath friendly... ah well.
