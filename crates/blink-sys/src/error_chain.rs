//! Rendering an error together with its sources.

use std::error::Error;
use std::fmt;

/// Displays an error and every [`source`](Error::source) below it, outermost
/// first, joined with `: `, without allocating.
///
/// blink's error types keep the underlying error (an `HRESULT`, an I/O
/// error) as a source rather than repeating it in their own message, so a
/// plain `%error` loses it. Log errors as `error = %ErrorChain(&error)`, and
/// build user-facing text with `ErrorChain(&error).to_string()`.
///
/// ```
/// use blink_sys::ErrorChain;
///
/// let error = std::fs::read("/nonexistent/blink").unwrap_err();
/// assert!(!ErrorChain(&error).to_string().is_empty());
/// ```
#[derive(Clone, Copy)]
pub struct ErrorChain<'a>(pub &'a (dyn Error + 'static));

impl fmt::Display for ErrorChain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)?;
        let mut source = self.0.source();
        while let Some(cause) = source {
            write!(f, ": {cause}")?;
            source = cause.source();
        }
        Ok(())
    }
}

impl fmt::Debug for ErrorChain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Layer(&'static str, Option<Box<Self>>);

    impl fmt::Display for Layer {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }

    impl Error for Layer {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            self.1.as_deref().map(|inner| {
                let source: &(dyn Error + 'static) = inner;
                source
            })
        }
    }

    #[test]
    fn lists_every_source() {
        let error = Layer(
            "failed to show the window",
            Some(Box::new(Layer(
                "no OpenGL context",
                Some(Box::new(Layer("driver missing", None))),
            ))),
        );
        assert_eq!(
            ErrorChain(&error).to_string(),
            "failed to show the window: no OpenGL context: driver missing"
        );
        assert_eq!(ErrorChain(&Layer("alone", None)).to_string(), "alone");
    }
}
