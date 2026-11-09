//! Wraps the whole window and reports where right-clicks happen, so context
//! menus can open at the cursor. Only right presses produce a message (not
//! every mouse move), and the click still reaches the widgets underneath.

use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{layout, mouse, overlay, renderer, Clipboard, Layout, Shell, Widget};
use iced::{event, Element, Event, Length, Point, Rectangle, Size, Vector};

pub struct Pointer<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    on_right_press: Box<dyn Fn(Point, Size) -> Message + 'a>,
}

/// `on_right_press` gets the cursor position and the window size.
pub fn pointer<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    on_right_press: impl Fn(Point, Size) -> Message + 'a,
) -> Pointer<'a, Message, Theme, Renderer> {
    Pointer { content: content.into(), on_right_press: Box::new(on_right_press) }
}

impl<Message, Theme, Renderer: renderer::Renderer> Widget<Message, Theme, Renderer> for Pointer<'_, Message, Theme, Renderer> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn layout(&self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        self.content.as_widget().layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(&self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.content.as_widget().operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn on_event(
        &mut self,
        tree: &mut Tree,
        event: Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) -> event::Status {
        // Automated tests: replay scripted input through the real widgets.
        if let (Some(fakes), Event::Mouse(_)) = (test_input::take(), &event) {
            for (fake, at) in fakes {
                let cursor = at.map_or(cursor, mouse::Cursor::Available);
                if let (Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)), Some(at)) = (&fake, at) {
                    shell.publish((self.on_right_press)(at, layout.bounds().size()));
                }
                let _ = self.content.as_widget_mut().on_event(&mut tree.children[0], fake, layout, cursor, renderer, clipboard, shell, viewport);
            }
            return event::Status::Captured;
        }
        // Published before the content sees the click, so the menu's position
        // arrives ahead of the message that opens it.
        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) = &event {
            if let Some(position) = cursor.position() {
                shell.publish((self.on_right_press)(position, layout.bounds().size()));
            }
        }
        self.content.as_widget_mut().on_event(&mut tree.children[0], event, layout, cursor, renderer, clipboard, shell, viewport)
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(&mut tree.children[0], layout, renderer, translation)
    }
}

impl<'a, Message: 'a, Theme: 'a, Renderer: renderer::Renderer + 'a> From<Pointer<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
{
    fn from(pointer: Pointer<'a, Message, Theme, Renderer>) -> Self {
        Element::new(pointer)
    }
}

/// Scripted input for automated tests, enabled by `FERROCHAT_TEST_INPUT=<file>`.
/// A test writes lines to the file and then sends the window any mouse
/// message; the lines are replayed through the widget tree as if they were
/// real input at those points, and the file is emptied. Lines:
/// `click X Y`, `rclick X Y`, `move X Y`, `wheel X Y LINES`, `type TEXT`,
/// `key enter|escape|up|down|tab|backspace`.
mod test_input {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use iced::keyboard::{self, key, Key, Location, Modifiers};
    use iced::mouse::{self, ScrollDelta};
    use iced::{Event, Point};

    pub fn take() -> Option<Vec<(Event, Option<Point>)>> {
        static FILE: OnceLock<Option<PathBuf>> = OnceLock::new();
        let path = FILE.get_or_init(|| std::env::var_os("FERROCHAT_TEST_INPUT").map(PathBuf::from)).as_ref()?;
        let script = std::fs::read_to_string(path).ok().filter(|s| !s.trim().is_empty())?;
        let _ = std::fs::write(path, "");
        Some(script.lines().flat_map(parse).collect())
    }

    fn press(key: Key, text: Option<&str>) -> Vec<(Event, Option<Point>)> {
        let pressed = keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key.clone(),
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: Location::Standard,
            modifiers: Modifiers::empty(),
            text: text.map(Into::into),
        };
        let released = keyboard::Event::KeyReleased { key, location: Location::Standard, modifiers: Modifiers::empty() };
        vec![(Event::Keyboard(pressed), None), (Event::Keyboard(released), None)]
    }

    fn parse(line: &str) -> Vec<(Event, Option<Point>)> {
        let mut words = line.split_whitespace();
        let command = words.next().unwrap_or_default();
        let rest: Vec<&str> = words.collect();
        let number = |i: usize| rest.get(i).and_then(|w| w.parse::<f32>().ok()).unwrap_or(0.0);
        let at = Some(Point::new(number(0), number(1)));
        let click = |button| {
            vec![
                (Event::Mouse(mouse::Event::CursorMoved { position: at.unwrap_or_default() }), at),
                (Event::Mouse(mouse::Event::ButtonPressed(button)), at),
                (Event::Mouse(mouse::Event::ButtonReleased(button)), at),
            ]
        };
        match command {
            "click" => click(mouse::Button::Left),
            "rclick" => click(mouse::Button::Right),
            "move" => vec![(Event::Mouse(mouse::Event::CursorMoved { position: at.unwrap_or_default() }), at)],
            "wheel" => vec![(Event::Mouse(mouse::Event::WheelScrolled { delta: ScrollDelta::Lines { x: 0.0, y: number(2) } }), at)],
            "type" => {
                let text = line.trim_start().strip_prefix("type").unwrap_or_default().trim_start_matches(' ');
                text.chars().flat_map(|c| press(Key::Character(c.to_string().into()), Some(&c.to_string()))).collect()
            }
            "key" => {
                let named = match rest.first().copied().unwrap_or_default() {
                    "enter" => key::Named::Enter,
                    "escape" => key::Named::Escape,
                    "up" => key::Named::ArrowUp,
                    "down" => key::Named::ArrowDown,
                    "tab" => key::Named::Tab,
                    "backspace" => key::Named::Backspace,
                    _ => return Vec::new(),
                };
                press(Key::Named(named), None)
            }
            _ => Vec::new(),
        }
    }
}
