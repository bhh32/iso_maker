mod app;

use crate::app::{theme, update, view};

fn main() -> iced::Result {
    iced::application("ISO Maker", update, view)
        .theme(theme)
        .run()
}
