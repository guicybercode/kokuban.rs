//! Track only PTY dimensions that the operating system accepted.
use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PtySize {
    pub columns: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl PtySize {
    pub(super) fn new(columns: u16, rows: u16, width: u32, height: u32) -> Self {
        Self {
            columns,
            rows,
            pixel_width: width.min(u32::from(u16::MAX)) as u16,
            pixel_height: height.min(u32::from(u16::MAX)) as u16,
        }
    }
}

#[derive(Default)]
pub(super) struct AppliedPtySize(Option<PtySize>);

impl AppliedPtySize {
    pub(super) fn apply(
        &mut self,
        size: PtySize,
        resize: impl FnOnce(PtySize) -> io::Result<()>,
    ) -> io::Result<()> {
        if self.0 != Some(size) {
            resize(size)?;
            self.0 = Some(size);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_only_changes_apply_without_changing_cells_or_repeating_ioctls() {
        let mut applied = AppliedPtySize::default();
        let mut observed = Vec::new();
        for (width, height) in [(800, 480), (800, 480), (809, 499), (809, 499)] {
            applied
                .apply(PtySize::new(80, 24, width, height), |size| {
                    observed.push(size);
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(
            observed,
            [
                PtySize::new(80, 24, 800, 480),
                PtySize::new(80, 24, 809, 499)
            ]
        );
    }

    #[test]
    fn failed_resize_retains_last_applied_dimensions_and_can_be_retried() {
        let mut applied = AppliedPtySize::default();
        let previous = PtySize::new(80, 24, 800, 480);
        let next = PtySize::new(100, 30, 1000, 600);
        applied.apply(previous, |_| Ok(())).unwrap();
        let failure = applied.apply(next, |_| Err(io::Error::other("resize failed")));
        assert!(failure.is_err());
        assert_eq!(applied.0, Some(previous));
        let mut retried = false;
        applied
            .apply(next, |_| {
                retried = true;
                Ok(())
            })
            .unwrap();
        assert!(retried);
        assert_eq!(applied.0, Some(next));
    }

    #[test]
    fn physical_area_includes_partial_cells_and_saturates_winsize_fields() {
        let size = PtySize::new(80, 24, 809, u32::MAX);
        assert_eq!((size.columns, size.rows), (80, 24));
        assert_eq!((size.pixel_width, size.pixel_height), (809, u16::MAX));
        let empty = PtySize::new(1, 1, 0, 0);
        assert_eq!((empty.pixel_width, empty.pixel_height), (0, 0));
    }
}
