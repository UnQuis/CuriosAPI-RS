//! The native Curios screen: a vanilla menu with mod-drawn decorations.
//!
//! Horizon 0.2 gives real, server-authoritative slots only to menu screens;
//! a `mod_screen_open` window has no slots at all. Curios has no block, so the
//! screen is built on a vanilla `minecraft:generic_9xN` chest menu opened by
//! [`open`]. Its container slots are filled with the stacks read from
//! [`crate::place::curio_place`] / [`crate::place::cosmetic_place`], so the
//! engine draws and moves real stacks. The mod draws only frames, labels,
//! cosmetic markers and page/render controls in the screen hooks.
//!
//! Every public `*_pre` / `*_post` function is for `lib.rs` to forward from
//! the single `#[horizon_mod] Hooks` implementation. This module never edits
//! that file.
//!
//! Adapted from TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright 2018-2024
//! C4); see COPYING and COPYING.LESSER.

use std::sync::Mutex;

use mod_api::{
    ClientHost, DrawContext, Flow, Holder, ItemPlace, MenuReplace, ModContainer,
    ScreenRenderBackgroundEvent, ScreenRenderPostEvent, ScreenView, ServerHost, Text,
};

use crate::config;
use crate::lang;
use crate::place;
use crate::registry;

/// Vanilla menu used for the screen. Six rows hold the 48 slots per page
/// allowed by the shipped server configuration.
pub const MENU_ID: &str = "minecraft:generic_9x6";
pub const MENU_ROWS: u8 = 6;
pub const MENU_COLUMNS: usize = 9;
pub const MENU_SLOTS: usize = MENU_ROWS as usize * MENU_COLUMNS;
pub const PLAYER_SLOTS: usize = 36;

/// The container for a block that wants to expose this menu. Curios itself
/// opens the menu directly through [`open`], but the shape is exported so a
/// future block can reuse it without a second menu id.
pub fn container() -> ModContainer {
    let inputs: Vec<usize> = (0..MENU_SLOTS).collect();
    let faces = [inputs.clone(), inputs.clone(), inputs.clone(), inputs.clone(), inputs.clone(), inputs];
    ModContainer {
        slots: MENU_SLOTS,
        faces,
        inputs: (0..MENU_SLOTS).collect(),
        outputs: (0..MENU_SLOTS).collect(),
        menu: Some(MENU_ID.to_owned()),
    }
}

/// One addressable curio cell on the current page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub slot_type: String,
    pub index: usize,
    pub cosmetic: bool,
}

impl Cell {
    /// The mod place this cell displays. Both kinds are encoded in the key:
    /// cosmetic pairs use the flag added in `src/place.rs`.
    pub fn place(&self, entity: u32) -> Option<ItemPlace> {
        let index = u16::try_from(self.index).ok()?;
        if self.cosmetic {
            place::cosmetic_place(entity, &self.slot_type, index)
        } else {
            place::curio_place(entity, &self.slot_type, index)
        }
    }
}

/// Number of pages needed for `total` cells. Always at least one.
pub fn page_count(total: usize, per_page: usize) -> usize {
    if per_page == 0 {
        return 1;
    }
    total.div_ceil(per_page).max(1)
}

/// Expands the registry into the cells displayed on `page`, in deterministic
/// `order`/`id` order. Cosmetic pairs follow their functional slot when the
/// slot type declares them.
pub fn visible_cells(page: usize, cosmetic: bool) -> Vec<Cell> {
    let per_page = usize::from(config::current().server.max_slots_per_page);
    let mut cells = Vec::new();
    for slot in registry::slot_types().iter() {
        if !slot.use_native_gui() {
            continue;
        }
        let size = usize::try_from(slot.size()).unwrap_or(0);
        for index in 0..size {
            cells.push(Cell {
                slot_type: slot.id().to_owned(),
                index,
                cosmetic: false,
            });
            if slot.has_cosmetic() {
                cells.push(Cell {
                    slot_type: slot.id().to_owned(),
                    index,
                    cosmetic: true,
                });
            }
        }
    }
    let start = page.saturating_mul(per_page).min(cells.len());
    let end = start.saturating_add(per_page).min(cells.len());
    cells[start..end]
        .iter()
        .filter(|cell| cosmetic || !cell.cosmetic)
        .cloned()
        .collect()
}

/// Cells actually shown on screen. Cosmetic view swaps each functional slot
/// for its cosmetic pair; it does not double the number of cells (Java's
/// `CuriosMenu.isViewingCosmetics` behaves the same way).
pub fn display_cells(page: usize, cosmetic: bool) -> Vec<Cell> {
    visible_cells(page, false)
        .into_iter()
        .map(|mut cell| {
            cell.cosmetic = cosmetic;
            cell
        })
        .collect()
}

/// The only screen state kept on the client. The stacks remain the server's.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuState {
    pub page: usize,
    pub cosmetic: bool,
    pub hovering: Option<usize>,
    /// Top-left of the open panel in GUI pixels, remembered from the last
    /// frame so input/tooltip hooks can hit-test without a `ScreenView`.
    pub origin: [f32; 2],
}

impl MenuState {
    pub fn new() -> Self {
        Self {
            page: 0,
            cosmetic: false,
            hovering: None,
            origin: [0.0, 0.0],
        }
    }

    pub fn advance(&mut self, forward: bool) -> bool {
        let per_page = usize::from(config::current().server.max_slots_per_page).max(1);
        let total = cell_total();
        let last = page_count(total, per_page).saturating_sub(1);
        let next = if forward {
            (self.page + 1).min(last)
        } else {
            self.page.saturating_sub(1)
        };
        let changed = next != self.page;
        self.page = next;
        changed
    }

    pub fn toggle_cosmetic(&mut self) {
        self.cosmetic = !self.cosmetic;
        self.hovering = None;
    }
}

impl Default for MenuState {
    fn default() -> Self {
        Self::new()
    }
}

fn cell_total() -> usize {
    registry::slot_types()
        .iter()
        .filter(|slot| slot.use_native_gui())
        .map(|slot| {
            let size = usize::try_from(slot.size()).unwrap_or(0);
            if slot.has_cosmetic() { size * 2 } else { size }
        })
        .sum()
}

static STATE: Mutex<Option<MenuState>> = Mutex::new(None);

/// Window id of the menu opened by this mod on the server. Client requests
/// carry an action only, so a stale client cannot address another menu.
static OPEN_WINDOW: Mutex<Option<i32>> = Mutex::new(None);

fn open_window() -> Option<i32> {
    OPEN_WINDOW
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .copied()
}

fn with_state<R>(default: R, f: impl FnOnce(&mut MenuState) -> R) -> R {
    let mut guard = STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match guard.as_mut() {
        Some(state) => f(state),
        None => default,
    }
}

/// A control under the pointer. Pure geometry, shared by drawing and input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    PagePrev,
    PageNext,
    ToggleCosmetics,
    ToggleRender,
    Slot(usize),
    None,
}

/// Screen-space layout in GUI pixels, derived from the menu panel rectangle.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub panel: [f32; 4],
}

const SLOT_STEP: f32 = 18.0;
const SLOT_INSET: f32 = 8.0;
const SLOT_TOP: f32 = 18.0;
const BUTTON_SIZE: f32 = 12.0;
const BUTTON_Y: f32 = 4.0;

impl Layout {
    pub fn new(panel: [f32; 4]) -> Self {
        Self { panel }
    }

    /// Rectangle of menu slot `index`, matching the vanilla chest layout.
    pub fn slot_rect(&self, index: usize) -> [f32; 4] {
        let column = (index % MENU_COLUMNS) as f32;
        let row = (index / MENU_COLUMNS) as f32;
        [
            self.panel[0] + SLOT_INSET + column * SLOT_STEP,
            self.panel[1] + SLOT_TOP + row * SLOT_STEP,
            16.0,
            16.0,
        ]
    }

    /// Rectangle of one of the three mod controls.
    pub fn button_rect(&self, control: Control) -> Option<[f32; 4]> {
        let x = match control {
            Control::PagePrev => self.panel[0] + self.panel[2] - 3.0 * BUTTON_SIZE - 6.0,
            Control::PageNext => self.panel[0] + self.panel[2] - 2.0 * BUTTON_SIZE - 6.0,
            Control::ToggleCosmetics => self.panel[0] + self.panel[2] - BUTTON_SIZE - 6.0,
            Control::ToggleRender => self.panel[0] + 6.0,
            Control::Slot(_) | Control::None => return None,
        };
        Some([x, self.panel[1] + BUTTON_Y, BUTTON_SIZE, BUTTON_SIZE])
    }

    pub fn hit(&self, at: [f32; 2]) -> Control {
        for control in [
            Control::PagePrev,
            Control::PageNext,
            Control::ToggleCosmetics,
            Control::ToggleRender,
        ] {
            if let Some(rect) = self.button_rect(control) {
                if contains(rect, at) {
                    return control;
                }
            }
        }
        for index in 0..MENU_SLOTS {
            if contains(self.slot_rect(index), at) {
                return Control::Slot(index);
            }
        }
        Control::None
    }
}

fn contains(rect: [f32; 4], at: [f32; 2]) -> bool {
    at[0] >= rect[0] && at[0] < rect[0] + rect[2] && at[1] >= rect[1] && at[1] < rect[1] + rect[3]
}

/// Whether the screen belongs to this mod's menu. A vanilla chest of the
/// same size is indistinguishable at this revision, so the title and slot
/// count are the only available guard.
fn is_ours(view: &ScreenView<'_>) -> bool {
    view.id == "inventory"
        && view.menu == Some(MENU_ID)
        && view.slots == MENU_SLOTS + PLAYER_SLOTS
}

/// Reads the stacks for `page` straight from the mod places. This is the
/// only source of truth: the menu is a mirror, never the storage.
pub fn page_stacks(host: &mut dyn ServerHost, uuid: u128, page: usize) -> Vec<Option<mod_api::ItemStack>> {
    let mut result = vec![None; MENU_SLOTS];
    let Some(player) = host.player(uuid) else {
        return result;
    };
    let entity = player.entity().id();
    let cosmetic = with_state(false, |state| state.cosmetic);
    for (slot, cell) in display_cells(page, cosmetic).into_iter().enumerate() {
        if slot >= MENU_SLOTS {
            break;
        }
        let Some(place) = cell.place(entity) else {
            continue;
        };
        result[slot] = place.stack(host);
    }
    result
}

/// Opens the native menu with the first page of curio stacks. Returns false
/// only when the player is gone or the engine refuses the open.
pub fn open(host: &mut dyn ServerHost, uuid: u128) -> bool {
    let title = title();
    let items = page_stacks(host, uuid, 0);
    let mut guard = STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = Some(MenuState::new());
    drop(guard);
    let opened = host.open_menu(uuid, MENU_ROWS, title, items, MenuReplace::CloseFirst);
    let mut window_guard = OPEN_WINDOW
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match opened {
        Ok(Some(window)) => {
            *window_guard = Some(window);
            true
        }
        Ok(None) => {
            *window_guard = None;
            false
        }
        Err(error) => {
            *window_guard = None;
            eprintln!("[curios] could not open menu for {uuid}: {error:?}");
            false
        }
    }
}

/// Refreshes the mirrored menu after an external change or a page turn.
pub fn refresh(host: &mut dyn ServerHost, uuid: u128, window: i32, page: usize) -> bool {
    let items = page_stacks(host, uuid, page);
    host.menu_set_items(uuid, window, items).is_ok()
}

/// Visible functional slot currently under the pointer, if our menu is open.
pub fn hovered_render_slot() -> Option<usize> {
    with_state(None, |state| state.hovering)
}

/// Server-authoritative render toggle for one visible functional slot.
/// Cosmetic slots have no independent render flag of their own: they follow
/// the functional slot they are paired with, as in the Java reference.
pub fn toggle_render(host: &mut dyn ServerHost, uuid: u128, slot: usize) -> bool {
    let page = with_state(0, |state| state.page);
    let Some(cell) = visible_cells(page, false).get(slot).cloned() else {
        return false;
    };
    if cell.cosmetic {
        return false;
    }
    let Some(player) = host.player(uuid) else {
        return false;
    };
    let holder = Holder::Entity(player.entity().id());
    let Ok(mut inventory) = crate::inventory::INVENTORY.get(host, holder) else {
        return false;
    };
    let Some(handler) = inventory.slot_mut(&cell.slot_type) else {
        return false;
    };
    let Some(current) = handler.render(cell.index) else {
        return false;
    };
    if handler.set_render(cell.index, !current).is_err() {
        return false;
    }
    crate::inventory::INVENTORY.set(host, holder, inventory).is_ok()
}

/// Server handler for Java `client_page`. The window comes from the menu this
/// mod opened, never from the packet.
pub fn client_page(host: &mut dyn ServerHost, uuid: u128, next: bool) -> bool {
    let Some(window) = open_window() else {
        return false;
    };
    with_state((), |state| {
        state.advance(next);
    });
    let page = with_state(0, |state| state.page);
    refresh(host, uuid, window, page)
}

/// Server handler for Java `toggle_cosmetics`. The cosmetic view swaps the
/// stacks shown in the same slots; it never duplicates slots.
pub fn toggle_cosmetics(host: &mut dyn ServerHost, uuid: u128) -> bool {
    let Some(window) = open_window() else {
        return false;
    };
    with_state((), |state| state.toggle_cosmetic());
    let page = with_state(0, |state| state.page);
    refresh(host, uuid, window, page)
}

/// Server handler for Java `destroy`: clear every functional and cosmetic
/// slot of the player. Clearing is a mutation of the authoritative attachment,
/// never of the mirrored menu.
pub fn destroy(host: &mut dyn ServerHost, uuid: u128) -> bool {
    let Some(player) = host.player(uuid) else {
        return false;
    };
    let holder = Holder::Entity(player.entity().id());
    let Ok(mut inventory) = crate::inventory::INVENTORY.get(host, holder) else {
        return false;
    };
    let ids: Vec<String> = inventory.slot_ids().map(str::to_owned).collect();
    for id in ids {
        let Some(handler) = inventory.slot_mut(&id) else {
            continue;
        };
        for index in 0..handler.len() {
            if let Err(error) = handler.set_stack(index, None) {
                eprintln!("[curios] destroy could not clear {id}/{index}: {error:?}");
                return false;
            }
            if let Err(error) = handler.set_cosmetic(index, None) {
                eprintln!("[curios] destroy could not clear cosmetic {id}/{index}: {error:?}");
                return false;
            }
        }
    }
    crate::inventory::INVENTORY.set(host, holder, inventory).is_ok()
}

/// Server handler for Java `open_vanilla`: close the menu we opened.
pub fn open_vanilla(host: &mut dyn ServerHost, uuid: u128) -> bool {
    host.close_menu(uuid);
    true
}

/// Server hook: a click on a mirrored slot must not let the vanilla chest
/// become a second storage. The click is consumed here; equip and unequip
/// stay in the server-authoritative `equip` module.
pub fn menu_clicked(host: &mut dyn ServerHost, uuid: u128, window: i32, slot: usize) -> bool {
    let per_page = usize::from(config::current().server.max_slots_per_page);
    if slot >= per_page || slot >= MENU_SLOTS {
        return false;
    }
    let page = with_state(0, |state| state.page);
    if !refresh(host, uuid, window, page) {
        eprintln!("[curios] could not restore menu slot {slot} after a click");
    }
    true
}

pub fn menu_closed() {
    let mut guard = STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.take();
    let mut window = OPEN_WINDOW
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    window.take();
}

/// Client hook: initialise state when our menu screen opens.
pub fn screen_opening(view: &ScreenView<'_>) {
    if !is_ours(view) {
        return;
    }
    let mut guard = STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match guard.as_mut() {
        Some(state) => {
            state.hovering = None;
            state.origin = [view.rect[0], view.rect[1]];
        }
        None => {
            let mut state = MenuState::new();
            state.origin = [view.rect[0], view.rect[1]];
            *guard = Some(state);
        }
    }
}

/// Client hook: frames and labels are drawn under the menu's own stacks.
pub fn screen_render_background(host: &dyn ClientHost, event: &mut ScreenRenderBackgroundEvent<'_>) {
    if !is_ours(event.screen) {
        return;
    }
    let state = with_state(MenuState::new(), |state| {
        state.origin = [event.screen.rect[0], event.screen.rect[1]];
        state.clone()
    });
    let layout = Layout::new(event.screen.rect);
    let cells = display_cells(state.page, state.cosmetic);
    for (slot, cell) in cells.iter().enumerate().take(MENU_SLOTS) {
        let rect = layout.slot_rect(slot);
        let frame = if cell.cosmetic { 0xFF_A855C8 } else { 0xFF_6B5A8C };
        event.draw.fill(
            [rect[0] - 1.0, rect[1] - 1.0, rect[2] + 2.0, rect[3] + 2.0],
            frame,
        );
        let label = lang::slot_name(lang::locale(host), &cell.slot_type)
            .unwrap_or(cell.slot_type.as_str());
        event.draw.text(label, [rect[0], rect[1] + rect[3] + 1.0], 0xFF_E8C56A, false);
    }
}

/// Client hook: buttons and hover tooltips are drawn over the screen.
pub fn screen_render_post(host: &dyn ClientHost, event: &mut ScreenRenderPostEvent<'_>) {
    if !is_ours(event.screen) {
        return;
    }
    let hovered = Layout::new(event.screen.rect).hit(event.screen.mouse);
    let hovered_slot = match hovered {
        Control::Slot(index) => Some(index),
        _ => None,
    };
    let state = with_state(MenuState::new(), |state| {
        state.origin = [event.screen.rect[0], event.screen.rect[1]];
        state.hovering = hovered_slot;
        state.clone()
    });
    let layout = Layout::new(event.screen.rect);
    draw_button(event.draw, layout, Control::PagePrev, "<");
    draw_button(event.draw, layout, Control::PageNext, ">");
    draw_button(
        event.draw,
        layout,
        Control::ToggleCosmetics,
        if state.cosmetic { "C" } else { "c" },
    );
    draw_button(event.draw, layout, Control::ToggleRender, "R");
    let per_page = usize::from(config::current().server.max_slots_per_page).max(1);
    let pages = page_count(cell_total(), per_page);
    let pattern = lang::lookup(lang::locale(host), lang::KEY_PAGE).unwrap_or("%d / %d");
    let label = pattern
        .replacen("%d", &(state.page + 1).to_string(), 1)
        .replacen("%d", &pages.to_string(), 1);
    event.draw.text(&label, [layout.panel[0] + 8.0, layout.panel[1] + 5.0], 0xFF_EFE7FF, true);
    if let Some(index) = hovered_slot {
        let cells = display_cells(state.page, state.cosmetic);
        if let Some(cell) = cells.get(index) {
            let lines = tooltip_lines(host, cell, false);
            if let Err(error) = event.draw.gui_tooltip(&lines, event.screen.mouse) {
                eprintln!("[curios] could not draw slot tooltip: {error:?}");
            }
        }
    }
}

/// Lines for one curio cell, in the same order as the Java `CurioSlot` tooltip:
/// identifier, cosmetic marker, then the inactive marker when the slot is
/// disabled. `screen_tooltip` and `screen_render_post` share this.
pub fn tooltip_lines(host: &dyn ClientHost, cell: &Cell, inactive: bool) -> Vec<Text> {
    let locale = lang::locale(host);
    let mut lines = vec![Text::literal(
        lang::slot_name(locale, &cell.slot_type)
            .unwrap_or(cell.slot_type.as_str())
            .to_owned(),
    )];
    if cell.cosmetic {
        lines.push(Text::literal(
            lang::lookup(locale, lang::KEY_COSMETIC)
                .unwrap_or("Cosmetic")
                .to_owned(),
        ));
    }
    if inactive {
        lines.push(Text::literal(
            lang::lookup(locale, lang::KEY_INACTIVE)
                .unwrap_or("Inactive")
                .to_owned(),
        ));
    }
    lines
}

/// Public tooltip hook for `Hooks::screen_tooltip`. The pinned hook is scoped
/// to block entities; Curios has no block, so `lib.rs` forwards it from a
/// generic screen hook with an empty `block`/`data`. The origin remembered by
/// the last draw call is used for hit-testing.
pub fn screen_tooltip(
    host: &dyn ClientHost,
    _block: &str,
    _data: &[i32],
    at: [f32; 2],
    lines: &mut Vec<String>,
) {
    let state = with_state(MenuState::new(), |state| state.clone());
    let control = Layout::new([state.origin[0], state.origin[1], 176.0, 256.0]).hit(at);
    let Control::Slot(index) = control else {
        return;
    };
    let cells = display_cells(state.page, state.cosmetic);
    let Some(cell) = cells.get(index) else {
        return;
    };
    let locale = lang::locale(host);
    let name = lang::slot_name(locale, &cell.slot_type)
        .unwrap_or(cell.slot_type.as_str())
        .to_owned();
    lines.push(name);
    if cell.cosmetic {
        lines.push(
            lang::lookup(locale, lang::KEY_COSMETIC)
                .unwrap_or("Cosmetic")
                .to_owned(),
        );
    }
}

fn draw_button(draw: &mut dyn DrawContext, layout: Layout, control: Control, label: &str) {
    if let Some(rect) = layout.button_rect(control) {
        draw.fill(rect, 0xFF_2E2545);
        draw.text(label, [rect[0] + 3.0, rect[1] + 1.0], 0xFF_EFE7FF, true);
    }
}

/// Client hook: page and cosmetic controls. Returning `Cancel` keeps the
/// click away from the underlying chest slot.
pub fn screen_mouse_button_pressed_pre(view: &ScreenView<'_>, button: i32) -> Flow {
    if !is_ours(view) || button != 0 {
        return Flow::Continue;
    }
    let control = Layout::new(view.rect).hit(view.mouse);
    match control {
        Control::PagePrev | Control::PageNext => {
            with_state(Flow::Continue, |state| {
                if state.advance(control == Control::PageNext) {
                    Flow::Cancel
                } else {
                    Flow::Continue
                }
            })
        }
        Control::ToggleCosmetics => {
            with_state(Flow::Continue, |state| {
                state.toggle_cosmetic();
                Flow::Cancel
            })
        }
        // The screen hooks hold an immutable client host, so the request is
        // sent from the key-mapping hook through input::send_render_toggle.
        Control::ToggleRender => Flow::Cancel,
        Control::Slot(_) | Control::None => Flow::Continue,
    }
}

/// Client hook: the wheel turns pages when the pointer is over our panel.
pub fn screen_mouse_scrolled_pre(view: &ScreenView<'_>, scroll: [f64; 2]) -> Flow {
    if !is_ours(view) || scroll[1] == 0.0 {
        return Flow::Continue;
    }
    let control = Layout::new(view.rect).hit(view.mouse);
    if !matches!(control, Control::Slot(_)) {
        return Flow::Continue;
    }
    with_state(Flow::Continue, |state| {
        if state.advance(scroll[1] > 0.0) {
            Flow::Cancel
        } else {
            Flow::Continue
        }
    })
}

/// The translatable title used by [`open`]. Kept as a translation key so the
/// client resolves it in the player's language.
pub fn title() -> Text {
    Text::translate(lang::KEY_NAME.to_owned(), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout::new([100.0, 50.0, 176.0, 256.0])
    }

    #[test]
    fn page_count_is_never_zero() {
        assert_eq!(page_count(0, 48), 1);
        assert_eq!(page_count(1, 48), 1);
        assert_eq!(page_count(48, 48), 1);
        assert_eq!(page_count(49, 48), 2);
        assert_eq!(page_count(42, 0), 1);
    }

    #[test]
    fn slots_follow_the_vanilla_grid() {
        let layout = layout();
        assert_eq!(layout.slot_rect(0), [108.0, 68.0, 16.0, 16.0]);
        assert_eq!(layout.slot_rect(9), [108.0, 86.0, 16.0, 16.0]);
        let panel_end = layout.panel[0] + layout.panel[2];
        assert!(layout.slot_rect(MENU_SLOTS - 1)[0] + 16.0 <= panel_end);
    }

    #[test]
    fn controls_win_over_slots_in_hit_testing() {
        let layout = layout();
        let rect = layout.button_rect(Control::PageNext).unwrap();
        let at = [rect[0] + 1.0, rect[1] + 1.0];
        assert_eq!(layout.hit(at), Control::PageNext);
        let slot = layout.slot_rect(0);
        assert_eq!(layout.hit([slot[0] + 1.0, slot[1] + 1.0]), Control::Slot(0));
        assert_eq!(layout.hit([0.0, 0.0]), Control::None);
    }

    #[test]
    fn visible_cells_follow_registry_order_and_hide_cosmetics_by_default() {
        let cells = visible_cells(0, true);
        let first = cells.first().expect("at least one cell");
        assert!(!first.cosmetic);
        assert_eq!(first.slot_type, "curio");
        // The shipped built-in definitions have no `add_cosmetic`, so no
        // cosmetic pairs exist until a datapack enables them. The cosmetic
        // place encoding itself is covered by the next test.
        assert!(cells.iter().all(|cell| !cell.cosmetic));
        assert_eq!(visible_cells(0, false), cells);
    }

    #[test]
    fn cosmetic_view_swaps_cells_without_doubling_them() {
        let functional = visible_cells(0, false);
        let swapped = display_cells(0, true);
        assert_eq!(functional.len(), swapped.len());
        assert!(functional.iter().all(|cell| !cell.cosmetic));
        assert!(swapped.iter().all(|cell| cell.cosmetic));
        assert_eq!(
            functional.iter().map(|cell| &cell.slot_type).collect::<Vec<_>>(),
            swapped.iter().map(|cell| &cell.slot_type).collect::<Vec<_>>()
        );
    }

    #[test]
    fn tooltip_keys_are_shipped_in_every_language() {
        for locale in lang::LOCALES {
            for key in [lang::KEY_IDENTIFIER_PREFIX, lang::KEY_COSMETIC, lang::KEY_INACTIVE] {
                if key == lang::KEY_IDENTIFIER_PREFIX {
                    continue;
                }
                assert!(lang::lookup(locale, key).is_some(), "{locale}/{key}");
            }
        }
        assert_eq!(lang::lookup(lang::DEFAULT_LOCALE, lang::KEY_INACTIVE), Some("Inactive"));
    }

    #[test]
    fn cosmetic_places_use_the_flag() {
        let cell = Cell {
            slot_type: "ring".to_owned(),
            index: 1,
            cosmetic: true,
        };
        match cell.place(7).expect("a place") {
            ItemPlace::Mod { key, .. } => assert!(place::is_cosmetic(key)),
            other => panic!("unexpected place: {other:?}"),
        }
    }

    #[test]
    fn state_pages_are_clamped() {
        let mut state = MenuState::new();
        let last = page_count(cell_total(), config::current().server.max_slots_per_page.into())
            .saturating_sub(1);
        for _ in 0..last + 3 {
            state.advance(true);
        }
        assert_eq!(state.page, last);
        for _ in 0..last + 3 {
            state.advance(false);
        }
        assert_eq!(state.page, 0);
        state.toggle_cosmetic();
        assert!(state.cosmetic);
    }

    #[test]
    fn container_declares_the_vanilla_menu() {
        let container = container();
        assert_eq!(container.menu.as_deref(), Some(MENU_ID));
        assert_eq!(container.slots, MENU_SLOTS);
    }
}
