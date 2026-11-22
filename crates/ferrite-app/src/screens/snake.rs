//! Snake (MENU 0 3 7). The game keeps no radio settings or saved state.

use super::shared::{R_NOTE, R_PAGE, R_ROW, R_TOP};
use super::{Cx, Hints, View};
use crate::chan::ChannelStore;
use crate::keys::{KeyEvent, Press};
use crate::{App, Screen};
use ferrite_core::keys::Key;
use ferrite_core::msg::LiveAll;
use ferrite_ui::assets::UI_14;
use ferrite_ui::gfx::*;
use ferrite_ui::panel::Panel;
use ferrite_ui::text::Buf;
use ferrite_ui::widgets as w;

const COLS: u8 = 16;
const ROWS: u8 = 13;
const CELLS: u8 = COLS * ROWS;
const START_LEN: u8 = 3;
const CELL: i32 = 14;
const X: i32 = 8;
const Y: i32 = 66;
const WIDTH: i32 = COLS as i32 * CELL;
const HEIGHT: i32 = ROWS as i32 * CELL;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl Direction {
    fn opposite(self, other: Self) -> bool {
        matches!((self, other), (Self::Up, Self::Down) | (Self::Down, Self::Up) | (Self::Left, Self::Right) | (Self::Right, Self::Left))
    }

    fn next(self, cell: u8) -> Option<u8> {
        let (x, y) = (cell % COLS, cell / COLS);
        match self {
            Self::Up if y > 0 => Some(cell - COLS),
            Self::Down if y + 1 < ROWS => Some(cell + COLS),
            Self::Left if x > 0 => Some(cell - 1),
            Self::Right if x + 1 < COLS => Some(cell + 1),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Ready,
    Playing,
    Paused,
    Lost,
    Won,
}

/// The tail-first ring and the corresponding occupied cells. Kept on App so BACK does not copy the board.
pub(crate) struct Game {
    body: [u8; CELLS as usize],
    occupied: [u16; ROWS as usize],
    tail: u8,
    len: u8,
    food: Option<u8>,
    direction: Direction,
    turn: Option<Direction>,
    phase: Phase,
    next: u32,
    rng: u32,
}

impl Game {
    pub(crate) const fn new() -> Self {
        Self {
            body: [0; CELLS as usize], occupied: [0; ROWS as usize], tail: 0, len: 0,
            food: None, direction: Direction::Right, turn: None, phase: Phase::Ready, next: 0, rng: 1,
        }
    }

    fn reset(&mut self, now: u32) {
        self.occupied.fill(0);
        self.tail = 0;
        self.len = START_LEN;
        let start = ROWS / 2 * COLS + 4;
        for i in 0..START_LEN {
            let cell = start + i;
            self.body[i as usize] = cell;
            self.occupied[(cell / COLS) as usize] |= Self::bit(cell);
        }
        self.direction = Direction::Right;
        self.turn = None;
        self.phase = Phase::Ready;
        self.rng = (self.rng ^ now).max(1);
        self.place_food();
    }

    fn bit(cell: u8) -> u16 {
        1 << (cell % COLS)
    }

    fn index(&self, offset: u8) -> usize {
        (u16::from(self.tail) + u16::from(offset)).rem_euclid(u16::from(CELLS)) as usize
    }

    fn head(&self) -> u8 {
        self.body[self.index(self.len - 1)]
    }

    fn contains(&self, cell: u8) -> bool {
        self.occupied[(cell / COLS) as usize] & Self::bit(cell) != 0
    }

    fn place_food(&mut self) {
        if self.len == CELLS {
            self.food = None;
            self.phase = Phase::Won;
            return;
        }
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        let index = (self.rng % u32::from(CELLS - self.len)) as usize;
        self.food = (0..CELLS).filter(|&cell| !self.contains(cell)).nth(index);
    }

    fn score(&self) -> u8 {
        self.len - START_LEN
    }

    pub(crate) fn playing(&self) -> bool {
        self.phase == Phase::Playing
    }

    fn start(&mut self, now: u32) {
        self.phase = Phase::Playing;
        self.schedule(now);
    }

    fn schedule(&mut self, now: u32) {
        let delay = 260u32.saturating_sub(u32::from(self.score()) * 6).max(105);
        self.next = now.wrapping_add(delay);
    }

    fn steer(&mut self, direction: Direction) {
        if self.phase == Phase::Playing && self.turn.is_none() && direction != self.direction && !self.direction.opposite(direction) {
            self.turn = Some(direction);
        }
    }

    fn advance(&mut self) {
        self.direction = self.turn.take().unwrap_or(self.direction);
        let Some(cell) = self.direction.next(self.head()) else {
            self.phase = Phase::Lost;
            return;
        };
        let eating = self.food == Some(cell);
        let tail = self.body[self.tail as usize];
        if self.contains(cell) && (eating || cell != tail) {
            self.phase = Phase::Lost;
            return;
        }
        let index = self.index(self.len);
        if eating {
            self.len += 1;
        } else {
            self.occupied[(tail / COLS) as usize] &= !Self::bit(tail);
            self.tail = (self.tail + 1) % CELLS;
        }
        self.body[index] = cell;
        self.occupied[(cell / COLS) as usize] |= Self::bit(cell);
        if eating {
            self.place_food();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Snake;

pub(crate) fn open(app: &mut App) -> Screen {
    app.snake.reset(app.now);
    Screen::Snake(Snake)
}

impl View for Snake {
    fn name(self) -> &'static str {
        "Snake"
    }

    fn own_arrows(self) -> bool {
        true
    }

    fn key(self, app: &mut App, event: KeyEvent, _: &impl ChannelStore) {
        if !matches!(event.press, Press::Short | Press::Repeat) {
            return;
        }
        let game = &mut app.snake;
        match event.key {
            Key::Up => game.steer(Direction::Up),
            Key::Down => game.steer(Direction::Down),
            Key::Left => game.steer(Direction::Left),
            Key::Right => game.steer(Direction::Right),
            Key::Menu if event.press == Press::Short => {
                match game.phase {
                    Phase::Playing => game.phase = Phase::Paused,
                    Phase::Ready | Phase::Paused => game.start(app.now),
                    Phase::Lost | Phase::Won => {
                        game.reset(app.now);
                        game.start(app.now);
                    }
                }
            }
            Key::Exit if event.press == Press::Short => app.back(),
            _ => {}
        }
    }

    fn tick(self, app: &mut App, _: &LiveAll) {
        if app.locked() {
            if app.snake.playing() {
                app.snake.phase = Phase::Paused;
            }
            return;
        }
        let game = &mut app.snake;
        if game.playing() && app.now.wrapping_sub(game.next) as i32 >= 0 {
            game.advance();
            game.schedule(app.now);
        }
    }

    fn render(self, app: &mut App, lcd: &mut impl Panel, _: &Cx<impl ChannelStore>) {
        if app.dirty(R_PAGE, 1) {
            lcd.fill(X - 1, Y - 1, WIDTH + 2, 1, LINE);
            lcd.fill(X - 1, Y + HEIGHT, WIDTH + 2, 1, LINE);
            lcd.fill(X - 1, Y, 1, HEIGHT, LINE);
            lcd.fill(X + WIDTH, Y, 1, HEIGHT, LINE);
        }
        let score = app.snake.score();
        if app.dirty(R_TOP, u32::from(score) + 1) {
            let mut label = Buf::<20>::new();
            let _ = ufmt::uwrite!(label, "Score {}", score);
            w::field(lcd, X, WIDTH, 28, 58, 49, label.as_bytes(), const { &w::st(&UI_14, TEXT, BG, Align::Center) });
        }
        let (head, food) = (app.snake.head(), app.snake.food);
        for row in 0..ROWS {
            let body = app.snake.occupied[row as usize];
            let head = (head / COLS == row).then_some(head % COLS);
            let food = food.filter(|&cell| cell / COLS == row).map(|cell| cell % COLS);
            let packed = u32::from(body) | (u32::from(head.unwrap_or(COLS)) << 16) | (u32::from(food.unwrap_or(COLS)) << 21);
            if app.dirty(R_ROW + row as usize, packed + 1) {
                draw_row(lcd, Y + i32::from(row) * CELL, body, head, food);
            }
        }
        let phase = app.snake.phase;
        if app.dirty(R_NOTE, phase as u32 + 1) {
            let (label, color): (&[u8], _) = match phase {
                Phase::Ready => (b"Use arrows to steer", SUB),
                Phase::Playing => (b"", SUB),
                Phase::Paused => (b"Paused", AMBER),
                Phase::Lost => (b"Game over", RED),
                Phase::Won => (b"Board complete", GREEN),
            };
            w::field(lcd, X, WIDTH, 258, 291, 281, label, &w::st(&UI_14, color, BG, Align::Center));
        }
    }

    fn hints(self, app: &App) -> Hints {
        let action = match app.snake.phase {
            Phase::Ready => "Start",
            Phase::Playing => "Pause",
            Phase::Paused => "Resume",
            Phase::Lost | Phase::Won => "Restart",
        };
        Hints::new(&[("OK", action), ("BACK", "Back")])
    }
}

fn draw_row(lcd: &mut impl Panel, y: i32, body: u16, head: Option<u8>, food: Option<u8>) {
    ferrite_ui::canvas::banded(lcd, X, y, WIDTH, CELL, SURFACE, |lcd| {
        for col in 0..COLS {
            let x = X + i32::from(col) * CELL;
            if food == Some(col) {
                lcd.fill(x + 5, y + 2, 4, 10, AMBER);
                lcd.fill(x + 2, y + 5, 10, 4, AMBER);
            } else if body & (1 << col) != 0 {
                if head == Some(col) {
                    lcd.fill(x + 1, y + 1, 12, 12, BLUE);
                    lcd.fill(x + 5, y + 5, 4, 4, SURFACE);
                } else {
                    lcd.fill(x + 1, y + 1, 12, 12, GREEN);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_2_eats_and_rejects_reversals() {
        let mut game = Game::new();
        game.reset(0);
        game.start(0);
        let cell = game.head() + 1;
        game.food = Some(cell);
        game.steer(Direction::Left);
        game.advance();
        assert_eq!(game.head(), cell);
        assert_eq!(game.score(), 1);
        assert!(game.food.is_some_and(|food| !game.contains(food)));
        game.steer(Direction::Down);
        game.steer(Direction::Up);
        game.advance();
        assert_eq!(game.head(), cell + COLS);
        assert_eq!(game.occupied.iter().map(|r| r.count_ones()).sum::<u32>(), 4);
    }

    #[test]
    fn snake_2_collision_allows_a_tail_cell_that_moves_away() {
        let mut game = Game::new();
        game.body[..4].copy_from_slice(&[17, 18, 34, 33]);
        game.occupied[1] = 0b110;
        game.occupied[2] = 0b110;
        game.len = 4;
        game.direction = Direction::Up;
        game.phase = Phase::Playing;
        game.advance();
        assert_eq!(game.head(), 17);
        assert_eq!(game.phase, Phase::Playing);
        game.body[..5].copy_from_slice(&[16, 17, 18, 34, 33]);
        game.tail = 0;
        game.len = 5;
        game.occupied[1] = 0b111;
        game.advance();
        assert_eq!(game.phase, Phase::Lost);
    }

    #[test]
    fn snake_3_board_uses_the_active_theme() {
        use ferrite_ui::{fb::FrameBuffer, theme};

        let previous = theme::get();
        for palette in 0..4 {
            theme::set(palette);
            let mut fb = FrameBuffer::new();
            draw_row(&mut fb, Y, 0b11, Some(1), Some(2));
            assert_eq!(fb.at(X + 1, Y + 1), theme::map(GREEN));
            assert_eq!(fb.at(X + CELL + 1, Y + 1), theme::map(BLUE));
            assert_eq!(fb.at(X + CELL * 2 + 5, Y + 2), theme::map(AMBER));
        }
        theme::set(previous);
    }

    #[test]
    fn snake_2_ends_when_every_cell_is_filled() {
        let mut game = Game::new();
        game.len = CELLS;
        game.place_food();
        assert_eq!(game.phase, Phase::Won);
        assert_eq!(game.food, None);
    }
}
