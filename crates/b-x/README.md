# b-x

b-x provides the stupidest boxed error ever.

When you don't want `eyre`, you don't want `thiserror`, you don't want `anyhow`,
you want much, much less. Something that just implements `std::error::Error`.

It's not even Send. You have to call `.bx()` on results and/or errors via extension
traits. It's so stupid.
