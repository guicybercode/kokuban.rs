pub mod ansi;
pub mod kitty_graphics;
pub mod osc_notify;
pub mod sixel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    EscapeIgnore,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    OscString,
    ApcString,
    DcsPassthrough,
}
