# gpui shim crate

This crate is required to allow us to use `gpui-ce` in place of `gpui-unofficial`.
`cntp-i18n` depends on `gpui-unofficial` because this is Victor's (@vicr123) preferred source of GPUI. This crate
exists for the sole purpose of allowing what ever GPUI version we want to use to pretend to be `gpui-unofficial`.
