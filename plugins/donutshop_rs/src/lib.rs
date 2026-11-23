//! DonutShop for HorizonX, in Rust, remade after Genius Shop (MIT, see NOTICE.md). `/shop` opens the main menu
//! from `config.yml`; each button opens a shop page from `shops/<key>.yml`. Left-click an item to buy it (a menu
//! with -10/-1/+1/+10/set buttons), right-click to sell it the same way. `/sell` opens a chest that pays for
//! what you put in. `/worth` tells what the held item is worth. Money is Essentials' (the Vault service).
//!
//! Prices move two ways: items with `dynamic-pricing` get dearer with each one bought and cheaper with each one
//! sold (kept between `min-price` and `max-price`), and the market moves every item's value on a timer (small
//! moves, sometimes a boom or a crash, then a drift back to normal). Selling never pays more than buying costs.
//!
//! Each item's price is in its name as well as its lore, and the player's money is in every menu's title, so
//! they show even where a tooltip's lore doesn't.

use horizon_bukkit::{CommandSender, ItemStack, Material, Player, Plugin, bukkit_plugin};
use horizon_kit::gui::{self, Menu};
use horizon_kit::hooks::economy;
use horizon_kit::{app, inv};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

const DEFAULT_CONFIG: &str = include_str!("../config.yml");
const DEFAULT_SHOPS: [(&str, &str); 7] = [
    ("blocks", include_str!("../shops/blocks.yml")),
    ("ores", include_str!("../shops/ores.yml")),
    ("farming", include_str!("../shops/farming.yml")),
    ("food", include_str!("../shops/food.yml")),
    ("mobs", include_str!("../shops/mobs.yml")),
    ("nether", include_str!("../shops/nether.yml")),
    ("special", include_str!("../shops/special.yml")),
];
const FILLER: &str = "GRAY_STAINED_GLASS_PANE";

// ---- Files ----

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Settings {
    pub main_menu: MainMenu,
    pub max_amount: i64,
    pub sell_title: String,
    pub market: MarketRules,
    pub messages: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct MainMenu {
    pub title: String,
    pub rows: usize,
    pub items: BTreeMap<String, Button>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Button {
    pub slot: usize,
    pub material: String,
    pub name: String,
    pub lore: Vec<String>,
    /// Which shop page it opens.
    pub shop_key: Option<String>,
    /// `shop` (default with a shop-key), `sell`, `command`, `close`; none = just shows.
    pub action: Option<String>,
    /// For `action: command`, run as the player, without the slash.
    pub command: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ShopFile {
    pub gui_name: String,
    pub rows: usize,
    pub items: Vec<ShopItem>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ShopItem {
    pub material: String,
    pub name: Option<String>,
    pub lore: Vec<String>,
    /// One item's price (0 = can't buy).
    pub price: f64,
    /// What one item pays back (0 = can't sell).
    pub sell_price: f64,
    /// Where the buy and sell menus start.
    pub amount: i64,
    pub dynamic_pricing: bool,
    pub price_change: f64,
    pub min_price: f64,
    pub max_price: f64,
}

impl ShopItem {
    pub fn title(&self) -> String {
        self.name.clone().unwrap_or_else(|| format!("&f{}", pretty(&self.material)))
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct MarketRules {
    pub enabled: bool,
    pub interval_minutes: u64,
    pub swing: f64,
    pub boom_chance: f64,
    pub crash_chance: f64,
    pub pull_back: f64,
    pub max: f64,
    pub min_buy: f64,
}

impl Default for MarketRules {
    fn default() -> Self {
        MarketRules { enabled: true, interval_minutes: 20, swing: 0.06, boom_chance: 0.02, crash_chance: 0.01, pull_back: 0.05, max: 10.0, min_buy: 0.1 }
    }
}

impl MarketRules {
    fn clamped(mut self) -> Self {
        self.interval_minutes = self.interval_minutes.max(1);
        self.swing = self.swing.clamp(0.0, 1.0);
        self.boom_chance = self.boom_chance.clamp(0.0, 1.0);
        self.crash_chance = self.crash_chance.clamp(0.0, 1.0);
        self.pull_back = self.pull_back.clamp(0.0, 1.0);
        self.max = self.max.max(1.0);
        self.min_buy = self.min_buy.clamp(0.0, 1.0);
        self
    }
}

/// Everything read from the files.
#[derive(Clone, Debug, Default)]
pub struct Shop {
    pub settings: Settings,
    /// (key, page) in file-name order.
    pub pages: Vec<(String, ShopFile)>,
    defaults: BTreeMap<String, String>,
}

impl Shop {
    pub fn parse(config: &str, pages: Vec<(String, String)>) -> (Shop, Vec<String>) {
        let mut problems = Vec::new();
        let default: Settings = serde_yaml::from_str(DEFAULT_CONFIG).unwrap_or_default();
        let mut settings: Settings = serde_yaml::from_str(config).unwrap_or_else(|e| {
            problems.push(format!("config.yml: {e} (using the built-in one)"));
            default.clone()
        });
        if settings.main_menu.items.is_empty() {
            settings.main_menu = default.main_menu.clone();
        }
        settings.main_menu.rows = settings.main_menu.rows.clamp(1, 6);
        settings.max_amount = if settings.max_amount <= 0 { 2304 } else { settings.max_amount.min(36 * 64) };
        settings.market = settings.market.clamped();
        let mut out = Vec::new();
        for (key, text) in pages {
            match serde_yaml::from_str::<ShopFile>(&text) {
                Ok(mut page) => {
                    page.rows = page.rows.clamp(2, 6);
                    page.items.retain(|i| !i.material.trim().is_empty());
                    for item in &mut page.items {
                        item.material = item.material.trim().to_ascii_uppercase();
                        item.price = item.price.max(0.0);
                        item.sell_price = item.sell_price.max(0.0);
                        item.amount = item.amount.max(1);
                    }
                    if page.gui_name.is_empty() {
                        page.gui_name = format!("&8{}", pretty(&key));
                    }
                    out.push((key, page));
                }
                Err(e) => problems.push(format!("shops/{key}.yml: {e} (left out)")),
            }
        }
        (Shop { settings, pages: out, defaults: default.messages }, problems)
    }

    pub fn item(&self, si: usize, ii: usize) -> Option<&ShopItem> {
        self.pages.get(si).and_then(|(_, p)| p.items.get(ii))
    }

    /// The first item the shop buys back as this material.
    pub fn sell_item(&self, material: &str) -> Option<&ShopItem> {
        self.pages.iter().flat_map(|(_, p)| &p.items).find(|i| i.material.eq_ignore_ascii_case(material) && i.sell_price > 0.0)
    }

    /// Any item of this material (for /worth).
    pub fn any_item(&self, material: &str) -> Option<&ShopItem> {
        self.sell_item(material).or_else(|| self.pages.iter().flat_map(|(_, p)| &p.items).find(|i| i.material.eq_ignore_ascii_case(material)))
    }

    pub fn page_index(&self, key: &str) -> Option<usize> {
        self.pages.iter().position(|(k, _)| k.eq_ignore_ascii_case(key))
    }

    pub fn msg(&self, key: &str, pairs: &[(&str, String)]) -> String {
        let mut text = self.settings.messages.get(key).or_else(|| self.defaults.get(key)).cloned().unwrap_or_else(|| key.to_owned());
        for (k, v) in pairs {
            text = text.replace(&format!("{{{k}}}"), v);
        }
        horizon_kit::text::colorize(&text)
    }

    fn materials(&self) -> Vec<String> {
        let mut all: Vec<String> = self.pages.iter().flat_map(|(_, p)| &p.items).map(|i| i.material.clone()).collect();
        all.sort();
        all.dedup();
        all
    }
}

// ---- Prices ----

/// The price after trades: `price + net * price-change`, kept in `min-price..=max-price` (0 = no limit).
/// `net` = items bought minus items sold. Items without dynamic pricing keep their price.
pub fn trade_price(it: &ShopItem, net: i64) -> f64 {
    if !it.dynamic_pricing {
        return it.price;
    }
    let mut p = it.price + net as f64 * it.price_change;
    if it.min_price > 0.0 {
        p = p.max(it.min_price);
    }
    if it.max_price > 0.0 {
        p = p.min(it.max_price);
    }
    p.max(0.0)
}

/// What `n` items cost bought at once: each one raises the next one's price, so a big buy pays its own rise.
pub fn buy_total(it: &ShopItem, net: i64, n: i64, value: f64, rules: &MarketRules) -> f64 {
    if !it.dynamic_pricing {
        return buy_unit(it, net, value, rules) * n as f64;
    }
    (0..n).map(|k| buy_unit(it, net + k, value, rules)).sum()
}

/// What `n` items pay sold at once: each one lowers the next one's price.
pub fn sell_total(it: &ShopItem, net: i64, n: i64, value: f64, rules: &MarketRules) -> f64 {
    if !it.dynamic_pricing {
        return sell_unit(it, net, value, rules) * n as f64;
    }
    (0..n).map(|k| sell_unit(it, net - k, value, rules)).sum()
}

/// One item's buy price now: after trades, times the market value (never below `min-buy` of normal).
pub fn buy_unit(it: &ShopItem, net: i64, value: f64, rules: &MarketRules) -> f64 {
    let v = if rules.enabled { value.max(rules.min_buy) } else { 1.0 };
    trade_price(it, net) * v
}

/// One item's sell price now: it moves with the buy price, and never pays more than buying costs.
pub fn sell_unit(it: &ShopItem, net: i64, value: f64, rules: &MarketRules) -> f64 {
    if it.sell_price <= 0.0 {
        return 0.0;
    }
    let ratio = if it.price > 0.0 { trade_price(it, net) / it.price } else { 1.0 };
    let v = if rules.enabled { value } else { 1.0 };
    let s = it.sell_price * ratio * v;
    if it.price > 0.0 { s.min(buy_unit(it, net, value, rules)) } else { s }
}

/// One market move of one value (1.0 = normal). `r1`, `r2` are random numbers in 0..1.
pub fn move_value(v: f64, rules: &MarketRules, r1: f64, r2: f64) -> f64 {
    if r1 < rules.crash_chance {
        // A crash lands where it lands (0 possible); the pull back starts from the next move.
        let v = v * 0.15 * r2;
        return if v < 0.01 { 0.0 } else { v };
    }
    let mut v = if r1 < rules.crash_chance + rules.boom_chance {
        v.max(0.05) * (1.8 + 2.2 * r2)
    } else {
        v * (1.0 + rules.swing * (2.0 * r2 - 1.0))
    };
    v += (1.0 - v) * rules.pull_back;
    if v < 0.01 { 0.0 } else { v.min(rules.max) }
}

/// Amount after a step, kept in `1..=max`.
pub fn step(amount: i64, by: i64, max: i64) -> i64 {
    (amount + by).clamp(1, max.max(1))
}

/// `OAK_LOG` -> `Oak Log`.
pub fn pretty(material: &str) -> String {
    material
        .split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let w = w.to_ascii_lowercase();
            let mut c = w.chars();
            c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A small random number source (xorshift), seeded from the clock.
struct Rng(u64);
impl Rng {
    fn new() -> Rng {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
        Rng(t | 1)
    }
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

// ---- Saved state: market values and trades ----

/// Market: material -> (value now, value before the last move). Trades: material -> bought minus sold.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub market: BTreeMap<String, (f64, f64)>,
    pub trades: BTreeMap<String, i64>,
    /// When the market last moved (Unix seconds), so restarts don't reset its clock.
    pub last_move: u64,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static SHOP: Mutex<Option<Arc<Shop>>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(STATE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(State::default))
}

fn state_file() -> std::path::PathBuf {
    app::folder().join("state.yml")
}

fn load_state() {
    let mut st: State = std::fs::read_to_string(state_file()).ok().and_then(|t| serde_yaml::from_str(&t).ok()).unwrap_or_default();
    // The first Rust DonutShop kept only the market, in market.yml.
    if st.market.is_empty()
        && let Some(old) = std::fs::read_to_string(app::folder().join("market.yml")).ok().and_then(|t| serde_yaml::from_str(&t).ok())
    {
        st.market = old;
    }
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(st);
}

// ponytail: the whole state file is written on every trade; fine for one server, batch it if it ever shows in TPS.
fn save_state() {
    let text = with_state(|s| serde_yaml::to_string(s).ok());
    if let Some(text) = text {
        let _ = std::fs::write(state_file(), text);
    }
}

fn net(material: &str) -> i64 {
    with_state(|s| s.trades.get(material).copied().unwrap_or(0))
}

fn value(material: &str) -> (f64, f64) {
    with_state(|s| s.market.get(material).copied().unwrap_or((1.0, 1.0)))
}

fn traded(material: &str, by: i64) {
    with_state(|s| *s.trades.entry(material.to_owned()).or_insert(0) += by);
    save_state();
}

fn buy_now(s: &Shop, it: &ShopItem) -> f64 {
    buy_unit(it, net(&it.material), value(&it.material).0, &s.settings.market)
}

fn sell_now(s: &Shop, it: &ShopItem) -> f64 {
    sell_unit(it, net(&it.material), value(&it.material).0, &s.settings.market)
}

fn buy_now_total(s: &Shop, it: &ShopItem, n: i64) -> f64 {
    buy_total(it, net(&it.material), n, value(&it.material).0, &s.settings.market)
}

fn sell_now_total(s: &Shop, it: &ShopItem, n: i64) -> f64 {
    sell_total(it, net(&it.material), n, value(&it.material).0, &s.settings.market)
}

/// A stack the shop buys back: plain (no name, lore, enchantments) and not worn down.
fn sellable(st: &ItemStack, material: &str) -> bool {
    inv::plain(st, material) && st.get_component("minecraft:damage").and_then(|d| d.as_i64()).unwrap_or(0) == 0
}

/// Called every minute: moves the market once its interval has passed since the last move (restarts included).
fn market_clock() {
    let every = shop().settings.market.interval_minutes * 60;
    let due = with_state(|st| {
        let now = now_secs();
        if st.last_move == 0 {
            st.last_move = now;
            false
        } else {
            now.saturating_sub(st.last_move) >= every
        }
    });
    if due {
        with_state(|st| st.last_move = now_secs());
        market_tick();
    }
}

/// `▲ +12%` / `▼ -40%` / `= 0%`: the last market move.
fn trend(material: &str) -> String {
    let (now, before) = value(material);
    let pct = if before > 0.0 { (now / before - 1.0) * 100.0 } else if now > 0.0 { 100.0 } else { 0.0 };
    if pct >= 0.5 {
        format!("&a▲ +{pct:.0}%")
    } else if pct <= -0.5 {
        format!("&c▼ {pct:.0}%")
    } else {
        "&7= 0%".to_owned()
    }
}

/// Every item's value moves once; the state is saved and the biggest moves are told to everyone.
fn market_tick() {
    let s = shop();
    if !s.settings.market.enabled {
        return;
    }
    let mut rng = Rng::new();
    let mut moves: Vec<(String, f64)> = Vec::new();
    with_state(|st| {
        for m in s.materials() {
            let (now, _) = st.market.get(&m).copied().unwrap_or((1.0, 1.0));
            let next = move_value(now, &s.settings.market, rng.next(), rng.next());
            st.market.insert(m.clone(), (next, now));
            moves.push((m, if now > 0.0 { next / now - 1.0 } else { 1.0 }));
        }
    });
    save_state();
    moves.sort_by(|a, b| a.1.total_cmp(&b.1));
    if let (Some(down), Some(up)) = (moves.first(), moves.last()) {
        let line = s.msg(
            "market-moved",
            &[("up", pretty(&up.0)), ("up_pct", format!("+{:.0}%", up.1 * 100.0)), ("down", pretty(&down.0)), ("down_pct", format!("{:.0}%", down.1 * 100.0))],
        );
        for p in horizon_bukkit::get_online_players() {
            p.send_message(&line);
        }
    }
}

// ---- Loading ----

fn shop() -> Arc<Shop> {
    SHOP.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(|| {
        let pages = DEFAULT_SHOPS.iter().map(|(k, t)| ((*k).to_owned(), (*t).to_owned())).collect();
        Arc::new(Shop::parse(DEFAULT_CONFIG, pages).0)
    })
}

/// Writes the default files where none are yet (never over the server's own), then reads them all.
fn load() -> Arc<Shop> {
    let dir = app::folder();
    let _ = std::fs::create_dir_all(&dir);
    let config = dir.join("config.yml");
    if !config.exists() {
        let _ = std::fs::write(&config, DEFAULT_CONFIG);
    }
    let shops = dir.join("shops");
    if !shops.exists() {
        let _ = std::fs::create_dir_all(&shops);
        for (key, text) in DEFAULT_SHOPS {
            let _ = std::fs::write(shops.join(format!("{key}.yml")), text);
        }
    }
    let (s, problems) = Shop::parse(&std::fs::read_to_string(&config).unwrap_or_default(), read_pages(&shops));
    for p in problems {
        app::warn(&p);
    }
    let s = Arc::new(s);
    *SHOP.lock().unwrap_or_else(|e| e.into_inner()) = Some(s.clone());
    s
}

fn read_pages(dir: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("yml")))
        .filter_map(|p| Some((p.file_stem()?.to_string_lossy().into_owned(), std::fs::read_to_string(&p).ok()?)))
        .collect();
    out.sort();
    out
}

fn money(v: f64) -> String {
    economy::format(v)
}

fn balance(p: &Player) -> String {
    money(economy::balance(p.get_name()))
}

/// `{balance}` filled in.
fn fill_in(text: &str, p: &Player) -> String {
    if text.contains("{balance}") { text.replace("{balance}", &balance(p)) } else { text.to_owned() }
}

fn strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

// ---- Menus ----

fn open_main(p: &Player) {
    let s = shop();
    let mm = &s.settings.main_menu;
    let mut m = Menu::new(mm.rows, &fill_in(&mm.title, p));
    m.tag("donutshop:main");
    let size = mm.rows * 9;
    for b in mm.items.values().filter(|b| b.slot < size) {
        let lore: Vec<String> = b.lore.iter().map(|l| fill_in(l, p)).collect();
        let icon = gui::icon(&b.material, &fill_in(&b.name, p), &strs(&lore));
        let action = b.action.clone().map(|a| a.to_ascii_lowercase()).or_else(|| b.shop_key.as_ref().map(|_| "shop".to_owned()));
        match action.as_deref() {
            Some("shop") => {
                let Some(si) = b.shop_key.as_deref().and_then(|k| s.page_index(k)) else {
                    m.item(b.slot, icon);
                    continue;
                };
                m.button(b.slot, icon, move |ctx| open_page(&ctx.player, si, 0));
            }
            Some("sell") => {
                m.button(b.slot, icon, |ctx| open_sell_chest(&ctx.player));
            }
            Some("close") => {
                m.button(b.slot, icon, |ctx| ctx.player.close_inventory());
            }
            Some("command") => {
                let cmd = b.command.clone().unwrap_or_default();
                m.button(b.slot, icon, move |ctx| {
                    ctx.player.close_inventory();
                    if !cmd.trim().is_empty() {
                        ctx.player.perform_command(cmd.trim().trim_start_matches('/'));
                    }
                });
            }
            _ => {
                m.item(b.slot, icon);
            }
        }
    }
    m.fill(gui::filler(FILLER));
    m.open(p);
}

/// An item's icon in a shop page: price and market move in its name too, so they show even without the lore.
fn item_icon(s: &Shop, it: &ShopItem) -> ItemStack {
    let (buy, sell) = (buy_now(s, it), sell_now(s, it));
    let mut name = it.title();
    if it.price > 0.0 {
        name.push_str(&format!(" &8| &aBuy {}", money(buy)));
    }
    if it.sell_price > 0.0 {
        name.push_str(&format!(" &8| &6Sell {}", money(sell)));
    }
    if s.settings.market.enabled {
        name.push_str(&format!(" &8| {}", trend(&it.material)));
    }
    let mut lore: Vec<String> = it.lore.clone();
    if it.price > 0.0 {
        lore.push(format!("&7Buy: &a{} &7each, &a{} &7for 64", money(buy), money(buy_now_total(s, it, 64))));
    }
    if it.sell_price > 0.0 {
        lore.push(format!("&7Sell: &6{} &7each, &6{} &7for 64", money(sell), money(sell_now_total(s, it, 64))));
    }
    if s.settings.market.enabled {
        lore.push(format!("&7Market: {}", trend(&it.material)));
    }
    if it.price > 0.0 {
        lore.push("&eLeft-click to buy".to_owned());
    }
    if it.sell_price > 0.0 {
        lore.push("&eRight-click to sell".to_owned());
    }
    gui::icon(&it.material, &name, &strs(&lore))
}

fn open_page(p: &Player, si: usize, page: usize) {
    let s = shop();
    let Some((_, sf)) = s.pages.get(si) else { return open_main(p) };
    let rows = sf.rows;
    let per = (rows - 1) * 9;
    let pages = sf.items.len().div_ceil(per).max(1);
    let page = page.min(pages - 1);
    let mut m = Menu::new(rows, &format!("{} &7| &a{}", sf.gui_name, balance(p)));
    m.tag(&format!("donutshop:page:{si}:{page}"));
    for (slot, (ii, it)) in sf.items.iter().enumerate().skip(page * per).take(per).enumerate() {
        m.button(slot, item_icon(&s, it), move |ctx| {
            if ctx.click.is_right() {
                open_trade(&ctx.player, si, ii, Trade::Sell, 0, page);
            } else if ctx.click.is_left() {
                open_trade(&ctx.player, si, ii, Trade::Buy, 0, page);
            }
        });
    }
    let b = per;
    m.button(b, gui::icon("ARROW", "&cBack", &[]), |ctx| open_main(&ctx.player));
    if page > 0 {
        m.button(b + 3, gui::icon("PAPER", &format!("&ePage {}", page), &[]), move |ctx| open_page(&ctx.player, si, page - 1));
    }
    m.item(b + 4, gui::icon("SUNFLOWER", &format!("&eYour money: &a{}", balance(p)), &[]));
    if page + 1 < pages {
        m.button(b + 5, gui::icon("PAPER", &format!("&ePage {}", page + 2), &[]), move |ctx| open_page(&ctx.player, si, page + 1));
    }
    // How to use it, in a name (names always show).
    m.item(b + 8, gui::icon("BOOK", "&eLeft-click: buy &8| &6Right-click: sell", &[]));
    for slot in b..b + 9 {
        if ![b, b + 3, b + 4, b + 5, b + 8].contains(&slot) {
            m.item(slot, gui::filler(FILLER));
        }
    }
    m.open(p);
}

#[derive(Clone, Copy, PartialEq)]
enum Trade {
    Buy,
    Sell,
}

/// Most of an item one player can buy now: what fits, what they can pay for, and the menu's cap.
fn most_affordable(s: &Shop, it: &ShopItem, p: &Player) -> i64 {
    let cap = inv::room_for(p, &it.material).min(s.settings.max_amount);
    let money = economy::balance(p.get_name());
    let (mut lo, mut hi) = (0i64, cap.max(0));
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if buy_now_total(s, it, mid) <= money + 1e-9 { lo = mid } else { hi = mid - 1 }
    }
    lo
}

/// The buy or sell menu for one item. `amount` 0 = start at the item's `amount`.
fn open_trade(p: &Player, si: usize, ii: usize, trade: Trade, amount: i64, back_page: usize) {
    let s = shop();
    let Some(it) = s.item(si, ii).cloned() else { return open_main(p) };
    let mat = it.material.clone();
    let have = inv::count(p, |st| sellable(st, &mat));
    let (unit, max) = match trade {
        Trade::Buy if it.price > 0.0 => (buy_now(&s, &it), s.settings.max_amount),
        Trade::Sell if it.sell_price > 0.0 => (sell_now(&s, &it), have.min(s.settings.max_amount)),
        Trade::Buy => {
            p.send_message(&s.msg("cant-buy", &[("item", pretty(&mat))]));
            return open_page(p, si, back_page);
        }
        Trade::Sell => {
            p.send_message(&s.msg("cant-sell", &[("item", pretty(&mat))]));
            return open_page(p, si, back_page);
        }
    };
    if trade == Trade::Sell && unit <= 0.0 {
        p.send_message(&s.msg("worthless", &[("item", pretty(&mat))]));
        return open_page(p, si, back_page);
    }
    if trade == Trade::Sell && have == 0 {
        p.send_message(&s.msg("no-items", &[("item", pretty(&mat))]));
        return open_page(p, si, back_page);
    }
    let amount = if amount <= 0 { it.amount } else { amount }.clamp(1, max.max(1));
    let total = match trade {
        Trade::Buy => buy_now_total(&s, &it, amount),
        Trade::Sell => sell_now_total(&s, &it, amount),
    };
    let verb = if trade == Trade::Buy { "Buy" } else { "Sell" };
    let extra = if trade == Trade::Sell { format!(" &7| you have {have}") } else { String::new() };
    let mut m = Menu::new(6, &format!("&8{verb} {} &7| &a{}{extra}", pretty(&mat), balance(p)));
    m.tag("donutshop:trade");

    let mut shown = ItemStack::new(Material::get_material(&mat).unwrap_or_else(|| Material::of("STONE")), amount.clamp(1, 64) as i32);
    horizon_kit::item::name(&mut shown, &format!("&f{amount}x {} &8| &a{} &7({} each)", pretty(&mat), money(total), money(unit)));
    let mut lore = vec![format!("&7Each: &f{}", money(unit)), format!("&7Total: &a{}", money(total))];
    if trade == Trade::Sell {
        lore.push(format!("&7You have: &f{have}"));
    }
    horizon_kit::item::lore(&mut shown, &lore);
    m.item(22, shown);

    for (slot, kind, n) in [(18, "set", 1), (19, "-", 10), (20, "-", 1), (24, "+", 1), (25, "+", 10), (26, "set", 64)] {
        let next = match kind {
            "set" => n.clamp(1, max.max(1)),
            "-" => step(amount, -n, max),
            _ => step(amount, n, max),
        };
        let (mat, label) = match kind {
            "set" => ("YELLOW_STAINED_GLASS_PANE", format!("&eSet to {n}")),
            "-" => ("RED_STAINED_GLASS_PANE", format!("&c-{n}")),
            _ => ("LIME_STAINED_GLASS_PANE", format!("&a+{n}")),
        };
        m.button(slot, gui::icon(mat, &label, &[]), move |ctx| open_trade(&ctx.player, si, ii, trade, next, back_page));
    }
    if trade == Trade::Buy {
        let most = most_affordable(&s, &it, p);
        if most > 0 {
            m.button(31, gui::icon("GOLD_INGOT", &format!("&6Max: {most} &7(what you can pay for and carry)"), &[]), move |ctx| {
                open_trade(&ctx.player, si, ii, trade, most, back_page)
            });
        } else {
            m.item(31, gui::icon("BARRIER", "&cYou can't pay for or carry even 1", &[]));
        }
    }
    let (confirm, back, cancel) = if trade == Trade::Buy { (39, 40, 41) } else { (48, 49, 50) };
    let label = format!("&a{verb} {amount} for {}", money(total));
    m.button(confirm, gui::icon("LIME_STAINED_GLASS", &label, &[]), move |ctx| {
        match trade {
            Trade::Buy => buy(&ctx.player, si, ii, amount, total),
            Trade::Sell => sell(&ctx.player, si, ii, amount, total),
        }
        open_trade(&ctx.player, si, ii, trade, amount, back_page);
    });
    if trade == Trade::Sell {
        let all_total = sell_now_total(&s, &it, have);
        let all = format!("&6Sell all {have} for {}", money(all_total));
        m.button(40, gui::icon("GOLD_BLOCK", &all, &[]), move |ctx| {
            sell(&ctx.player, si, ii, have, all_total);
            open_page(&ctx.player, si, back_page);
        });
    }
    m.button(back, gui::icon("ENDER_CHEST", "&eBack", &[]), move |ctx| open_page(&ctx.player, si, back_page));
    m.button(cancel, gui::icon("RED_STAINED_GLASS", "&cClose", &[]), |ctx| ctx.player.close_inventory());
    m.fill(gui::filler(FILLER));
    m.open(p);
}

/// Where the sell chest's green button sits; it's never paid for or handed out.
const SELL_BUTTON: usize = 49;

fn open_sell_chest(p: &Player) {
    let s = shop();
    let mut m = Menu::new(6, &format!("{} &7| &a{}", s.settings.sell_title, balance(p)));
    m.tag("donutshop:sell");
    m.allow_take(true);
    m.button(SELL_BUTTON, gui::icon("LIME_STAINED_GLASS", "&aSell everything in here &7(or just close it)", &[]), |ctx| ctx.player.close_inventory());
    m.on_close_items(|player, items| sell_chest(player, items));
    m.open(p);
}

// ---- Buying and selling ----

/// Prices can move while a menu is open (market tick, other players): a total off by more than a cent from
/// what the button showed is refused, and the menu reopens with the new one.
const CENT: f64 = 0.005;

fn buy(p: &Player, si: usize, ii: usize, amount: i64, shown: f64) {
    let s = shop();
    let Some(it) = s.item(si, ii).cloned() else { return };
    let name = pretty(&it.material);
    if !economy::enabled() {
        return p.send_message(&s.msg("no-economy", &[]));
    }
    if inv::room_for(p, &it.material) < amount {
        return p.send_message(&s.msg("no-room", &[("amount", amount.to_string()), ("item", name)]));
    }
    let total = buy_now_total(&s, &it, amount);
    if total > shown + CENT {
        return p.send_message(&s.msg("price-changed", &[("price", money(total))]));
    }
    if !economy::withdraw(p.get_name(), total) {
        return p.send_message(&s.msg("no-money", &[("price", money(total)), ("balance", balance(p))]));
    }
    inv::give_many(p, &it.material, amount);
    traded(&it.material, amount);
    p.send_message(&s.msg("bought", &[("amount", amount.to_string()), ("item", name), ("price", money(total))]));
}

fn sell(p: &Player, si: usize, ii: usize, amount: i64, shown: f64) {
    let s = shop();
    let Some(it) = s.item(si, ii).cloned() else { return };
    if !economy::enabled() {
        return p.send_message(&s.msg("no-economy", &[]));
    }
    // A crashed item pays nothing: keep the player's items instead of taking them for $0.
    if sell_now(&s, &it) <= 0.0 {
        return p.send_message(&s.msg("worthless", &[("item", pretty(&it.material))]));
    }
    let mat = it.material.clone();
    let have = inv::count(p, |st| sellable(st, &mat));
    let n = amount.clamp(0, have);
    if n == 0 {
        return p.send_message(&s.msg("no-items", &[("item", pretty(&mat))]));
    }
    let pay = sell_now_total(&s, &it, n);
    if n == amount && pay + CENT < shown {
        return p.send_message(&s.msg("price-changed", &[("price", money(pay))]));
    }
    let (n, taken) = inv::take(p, |st| sellable(st, &mat), n);
    let pay = sell_now_total(&s, &it, n);
    if !economy::deposit(p.get_name(), pay) {
        for st in &taken {
            inv::give(p, st);
        }
        return p.send_message(&s.msg("no-economy", &[]));
    }
    traded(&mat, -n);
    p.send_message(&s.msg("sold", &[("amount", n.to_string()), ("price", money(pay))]));
}

/// The sell chest closed: plain, undamaged items with a sell price are paid for, everything else goes back.
fn sell_chest(p: &Player, items: Vec<Option<ItemStack>>) {
    let s = shop();
    let eco = economy::enabled();
    let mut sold: BTreeMap<String, (i64, Vec<ItemStack>)> = BTreeMap::new();
    for (slot, st) in items.into_iter().enumerate() {
        let Some(st) = st else { continue };
        if slot == SELL_BUTTON {
            continue;
        }
        let mat = st.get_type().name().to_ascii_uppercase();
        let pays = eco && s.sell_item(&mat).is_some_and(|i| sell_now(&s, i) > 0.0);
        if pays && sellable(&st, &mat) {
            let e = sold.entry(mat).or_default();
            e.0 += i64::from(st.get_amount());
            e.1.push(st);
        } else {
            inv::give(p, &st);
        }
    }
    if !eco {
        return p.send_message(&s.msg("no-economy", &[]));
    }
    if sold.is_empty() {
        return p.send_message(&s.msg("sold-none", &[]));
    }
    let pay: f64 = sold.iter().filter_map(|(m, (n, _))| s.sell_item(m).map(|i| sell_now_total(&s, i, *n))).sum();
    let count: i64 = sold.values().map(|(n, _)| n).sum();
    if !economy::deposit(p.get_name(), pay) {
        for (_, stacks) in sold.values() {
            for st in stacks {
                inv::give(p, st);
            }
        }
        return p.send_message(&s.msg("no-economy", &[]));
    }
    with_state(|st| {
        for (m, (n, _)) in &sold {
            *st.trades.entry(m.clone()).or_insert(0) -= n;
        }
    });
    save_state();
    p.send_message(&s.msg("sold", &[("amount", count.to_string()), ("price", money(pay))]));
}

fn worth(p: &Player) {
    let s = shop();
    let Some(held) = p.get_inventory().get_item_in_main_hand().filter(|h| h.get_amount() > 0) else {
        return p.send_message(&s.msg("worth-empty", &[]));
    };
    let mat = held.get_type().name().to_ascii_uppercase();
    let Some(it) = s.any_item(&mat) else {
        return p.send_message(&s.msg("worth-none", &[("item", pretty(&mat))]));
    };
    let (buy, sell) = (buy_now(&s, it), sell_now(&s, it));
    let n = i64::from(held.get_amount());
    let none = || "-".to_owned();
    p.send_message(&s.msg(
        "worth",
        &[
            ("item", pretty(&mat)),
            ("sell", if it.sell_price > 0.0 { money(sell) } else { none() }),
            ("stack", if it.sell_price > 0.0 { money(sell * n as f64) } else { none() }),
            ("amount", n.to_string()),
            ("buy", if it.price > 0.0 { money(buy) } else { none() }),
        ],
    ));
}

// ---- Commands ----

fn shop_command(sender: &CommandSender, _label: &str, args: &[&str]) -> bool {
    match args.first().map(|a| a.to_ascii_lowercase()).as_deref() {
        Some("reload") => {
            if !sender.has_permission("donutshop.admin") {
                sender.send_message("§cYou can't do that.");
                return true;
            }
            gui::close_all();
            let s = load();
            let items: usize = s.pages.iter().map(|(_, p)| p.items.len()).sum();
            sender.send_message(&s.msg("reloaded", &[("shops", s.pages.len().to_string()), ("items", items.to_string())]));
        }
        Some("sell") => match sender.as_player() {
            Some(p) => open_sell_chest(p),
            None => sender.send_message("Only players can sell."),
        },
        _ => match sender.as_player() {
            Some(p) => open_main(p),
            None => sender.send_message("Only players can open the shop."),
        },
    }
    true
}

fn on_enable(plugin: Plugin) {
    app::set(plugin);
    gui::install(plugin);
    let s = load();
    load_state();
    plugin.set_executor("shop", shop_command);
    plugin.set_tab_completer("shop", |s, _, a| {
        let mut out = vec!["sell".to_owned()];
        if s.has_permission("donutshop.admin") {
            out.push("reload".to_owned());
        }
        let t = a.first().map(|x| x.to_ascii_lowercase()).unwrap_or_default();
        if a.len() <= 1 { out.into_iter().filter(|o| o.starts_with(&t)).collect() } else { Vec::new() }
    });
    plugin.set_executor("sell", |sender, _, _| {
        match sender.as_player() {
            Some(p) => open_sell_chest(p),
            None => sender.send_message("Only players can sell."),
        }
        true
    });
    plugin.set_executor("worth", |sender, _, _| {
        match sender.as_player() {
            Some(p) => worth(p),
            None => sender.send_message("Only players hold items."),
        }
        true
    });
    // The economy shows up once Essentials is enabled; ask again then.
    app::later(40, || horizon_kit::hooks::refresh());
    // Checked every minute against the saved time of the last move, so restarts don't reset the market's clock.
    plugin.run_task_timer(1200, 1200, market_clock);
    put_pause_button();
    let items: usize = s.pages.iter().map(|(_, p)| p.items.len()).sum();
    plugin.info(&format!("DonutShop (Rust, after Genius Shop) ready: {} shops, {items} items", s.pages.len()));
}

/// The "Donut" button in the Escape menu is a data pack (a dialog in the `pause_screen_additions` tag): written
/// into the world's datapacks once, so it shows from the next server start. Never overwritten (edit it freely).
fn put_pause_button() {
    let root = app::folder().parent().and_then(|p| p.parent()).map(|p| p.to_path_buf()).unwrap_or_default();
    let level = std::fs::read_to_string(root.join("server.properties"))
        .ok()
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("level-name=").map(|v| v.trim().to_owned())))
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "world".to_owned());
    let pack = root.join(level).join("datapacks").join("donutmenu");
    if pack.exists() {
        return;
    }
    for (rel, text) in [
        ("pack.mcmeta", include_str!("../pack/pack.mcmeta")),
        ("data/minecraft/tags/dialog/pause_screen_additions.json", include_str!("../pack/data/minecraft/tags/dialog/pause_screen_additions.json")),
        ("data/donut/dialog/menu.json", include_str!("../pack/data/donut/dialog/menu.json")),
    ] {
        let path = pack.join(rel);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, text);
    }
    app::info("Added the Donut button to the Escape menu; it shows after the next restart.");
}

fn on_disable(_plugin: Plugin) {
    gui::close_all();
    app::clear();
}

bukkit_plugin!(on_enable, on_disable);

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Shop {
        let pages = DEFAULT_SHOPS.iter().map(|(k, t)| ((*k).to_owned(), (*t).to_owned())).collect();
        let (s, problems) = Shop::parse(DEFAULT_CONFIG, pages);
        assert!(problems.is_empty(), "{problems:?}");
        s
    }

    #[test]
    fn default_files_read_and_every_button_leads_somewhere() {
        let s = defaults();
        assert_eq!(s.pages.len(), 7);
        let mm = &s.settings.main_menu;
        let mut slots = std::collections::HashSet::new();
        for (key, b) in &mm.items {
            assert!(b.slot < mm.rows * 9, "{key} is off the menu");
            assert!(slots.insert(b.slot), "{key} shares a slot");
            if let Some(k) = &b.shop_key {
                assert!(s.page_index(k).is_some(), "{key} opens a missing shop {k}");
            }
        }
        for (key, p) in &s.pages {
            assert!(!p.items.is_empty(), "{key} is empty");
            for it in &p.items {
                assert!(Material::of(&it.material).key().starts_with("minecraft:"));
                assert!(it.price > 0.0 || it.sell_price > 0.0, "{} can't be traded", it.material);
                assert!(it.sell_price <= it.price || it.price == 0.0, "{} sells above its price", it.material);
            }
        }
        let dirt = s.sell_item("dirt").unwrap();
        assert_eq!((dirt.price, dirt.sell_price), (1.0, 0.1));
        assert!(s.sell_item("ELYTRA").is_none()); // buy-only
        assert!(s.any_item("ELYTRA").is_some());
        assert_eq!(s.settings.max_amount, 2304);
        assert!(s.msg("bought", &[("amount", "3".into()), ("item", "Dirt".into()), ("price", "$3".into())]).contains("3x Dirt"));
        assert_eq!(s.msg("worth-empty", &[]), s.msg("worth-empty", &[])); // falls back to the built-in text
    }

    #[test]
    fn prices_follow_trades_and_the_market_within_bounds() {
        let s = defaults();
        let r = &s.settings.market;
        let diamond = s.sell_item("DIAMOND").unwrap();
        assert!(diamond.dynamic_pricing);
        // Buying raises, selling lowers, bounds hold.
        assert!(trade_price(diamond, 100) > trade_price(diamond, 0));
        assert!(trade_price(diamond, -100) < trade_price(diamond, 0));
        assert_eq!(trade_price(diamond, 1_000_000), diamond.max_price);
        assert_eq!(trade_price(diamond, -1_000_000), diamond.min_price);
        // A big buy pays its own price rise; a big sale gets its own price drop; a buy-then-sell at once loses.
        let one = buy_unit(diamond, 0, 1.0, r);
        assert!(buy_total(diamond, 0, 64, 1.0, r) > one * 64.0);
        assert!(sell_total(diamond, 64, 64, 1.0, r) < sell_unit(diamond, 64, 1.0, r) * 64.0);
        for n in [1, 64, 2304] {
            assert!(sell_total(diamond, n, n, 1.0, r) < buy_total(diamond, 0, n, 1.0, r), "buying then selling {n} pays");
        }
        // Without dynamic pricing, trades change nothing.
        let dirt = s.sell_item("DIRT").unwrap();
        assert!((buy_total(dirt, 0, 64, 1.0, r) - dirt.price * 64.0).abs() < 1e-9);
        assert_eq!(trade_price(dirt, 5000), dirt.price);
        // Market: a crashed item still costs min-buy to buy, and sells for nothing.
        assert!((buy_unit(dirt, 0, 0.0, r) - dirt.price * r.min_buy).abs() < 1e-9);
        assert_eq!(sell_unit(dirt, 0, 0.0, r), 0.0);
        // Selling never pays more than buying costs, at any market value or trade count.
        for v in [0.0, 0.5, 1.0, 3.0, 10.0] {
            for n in [-100_000, 0, 100_000] {
                assert!(sell_unit(diamond, n, v, r) <= buy_unit(diamond, n, v, r) + 1e-9);
            }
        }
        // Market moves: steady stays near, a crash can reach 0, a boom goes up, max holds, 0 drifts back.
        let steady = move_value(1.0, r, 0.5, 0.9);
        assert!(steady > 0.95 && steady < 1.1, "{steady}");
        assert_eq!(move_value(0.05, r, 0.0, 0.0), 0.0);
        assert!(move_value(1.0, r, r.crash_chance + 0.001, 1.0) > 3.0);
        assert!(move_value(9.9, r, r.crash_chance + 0.001, 1.0) <= r.max);
        let mut v = 0.0;
        for _ in 0..40 {
            v = move_value(v, r, 0.5, 0.5);
        }
        assert!(v > 0.5, "a crashed item comes back: {v}");
        assert_eq!(step(1, -64, 2304), 1);
        assert_eq!(step(60, 64, 2304), 124);
        assert_eq!(step(2304, 1, 2304), 2304);
        assert_eq!(step(5, 10, 7), 7);
        assert_eq!(pretty("OAK_LOG"), "Oak Log");
        // A broken shop file is left out and named, the rest still load.
        let (bad, problems) = Shop::parse(DEFAULT_CONFIG, vec![("x".into(), "items: [nope".into()), ("blocks".into(), DEFAULT_SHOPS[0].1.into())]);
        assert_eq!(bad.pages.len(), 1);
        assert_eq!(problems.len(), 1);
    }

    /// The game reads each item component as SNBT and silently drops one it can't read; a dropped lore is a
    /// tooltip with no prices. Feed a shop icon's name and lore (market arrows, money with commas) through the
    /// game's own reader.
    #[test]
    fn shop_icon_text_survives_the_games_snbt_reader() {
        use horizon_world::nbt::{Tag, parse_snbt};
        let mut st = ItemStack::new(Material::of("DIRT"), 1);
        horizon_kit::item::name(&mut st, "&6Dirt &8| &aBuy $1,234.50 &8| &6Sell $0.10");
        let lore: Vec<String> = ["&7Buy: &a$1,234.50 &7each", "&7Market: &a▲ +12%", "&7Market: &c▼ -40%", "&7Market: &7= 0%", "&eLeft-click to buy"].map(String::from).to_vec();
        horizon_kit::item::lore(&mut st, &lore);
        let json = st.to_json();
        let comps = json["components"].as_object().expect("components");
        for key in ["minecraft:custom_name", "minecraft:lore"] {
            let snbt = comps[key].as_str().expect("an SNBT string");
            println!("{key} = {snbt}");
            let (tag, _) = parse_snbt(snbt).unwrap_or_else(|e| panic!("the game can't read {key}: {e}\n{snbt}"));
            if key == "minecraft:lore" {
                let Tag::List(lines) = tag else { panic!("lore isn't a list: {tag:?}") };
                assert_eq!(lines.len(), lore.len());
            }
        }
    }
}
