pub mod ansi;
pub mod kitty_graphics;
pub mod sixel;
pub mod osc_notify;

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
