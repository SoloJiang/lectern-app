# login_env fixtures

`vars.bin`: the environment a fake login shell replays in
`src/login_env/tests.rs`, as `env -0` prints it (`name=value` entries, each
ending in NUL). It has a value with a newline, a value with `=`, a value that
is not UTF-8, an empty value and a value with a single quote. Synthetic; no
real environment.
