## Contributing to fluke

fluke is still experimental, but you can help by trying to build
stuff with it.

The highest-impact work right now is probably picking up an h2spec
case and making it pass, or tackling some of the `TODO:` or `FIXME:`
in the code.

More test coverage is always welcome, it's tracked via llvm-cov, GitHub
actions will comment on PR with coverage changes, and you can see the
coverage for yourself with `just cov`, in the `coverage/html` directory.

Some more design needs to happen, especially re: configuration and
introspection of running HTTP services, that'll happen when H1/H2
work relatively well.
