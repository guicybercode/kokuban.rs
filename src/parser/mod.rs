pub mod ansi;
pub mod kitty_graphics;
pub mod sixel;
pub mod osc_notify;

#[derive(Debug, Clone)]
pub enum Action {
    Print(char),
    Execute(u8),
    CsiDispatch {
        params: Vec<u16>,
        intermediates: Vec<u8>,
        final_byte: u8,
    },
    OscDispatch(Vec<Vec<u8>>),
}

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
