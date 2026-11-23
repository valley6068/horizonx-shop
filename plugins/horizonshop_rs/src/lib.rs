//! HorizonShop for HorizonX, in Rust, remade after Genius Shop (MIT, see NOTICE.md). `/shop` opens the main menu
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
    m.tag("horizonshop:main");
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
            Some("pay") => {
                m.button(b.slot, icon, |ctx| open_pay_players(&ctx.player));
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
    m.tag(&format!("horizonshop:page:{si}:{page}"));
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
    m.tag("horizonshop:trade");

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

// ---- Paying players ----

/// Online players to pay, as heads.
fn open_pay_players(p: &Player) {
    let me = p.get_name().to_owned();
    let others: Vec<String> = horizon_bukkit::get_online_players().iter().map(|o| o.get_name().to_owned()).filter(|n| *n != me).collect();
    let mut m = Menu::new(6, &format!("&8Pay a player &7| &a{}", balance(p)));
    m.tag("horizonshop:pay");
    if others.is_empty() {
        m.item(22, gui::icon("BARRIER", "&7Nobody else is online", &[]));
    }
    for (slot, name) in others.into_iter().take(45).enumerate() {
        let mut head = horizon_kit::item::head(&name);
        horizon_kit::item::name(&mut head, &format!("&f{name} &8| &eClick to pay"));
        m.button(slot, head, move |ctx| open_pay_amount(&ctx.player, &name, 0));
    }
    m.button(49, gui::icon("ARROW", "&cBack", &[]), |ctx| open_main(&ctx.player));
    m.fill(gui::filler(FILLER));
    m.open(p);
}

/// How much to send `to`: steps, a "pay" button, and for moderators a "give from the server" button.
fn open_pay_amount(p: &Player, to: &str, amount: i64) {
    let have = economy::balance(p.get_name()).floor().max(0.0) as i64;
    let amount = amount.max(0);
    let mut m = Menu::new(6, &format!("&8Pay {to} &7| &a{}", balance(p)));
    m.tag("horizonshop:payamount");
    let mut shown = horizon_kit::item::head(to);
    horizon_kit::item::name(&mut shown, &format!("&f{to} &8| &a{}", money(amount as f64)));
    m.item(13, shown);
    for (slot, by) in [(18, -1000), (19, -100), (20, -10), (21, -1), (23, 1), (24, 10), (25, 100), (26, 1000)] {
        let next = (amount + by).max(0);
        let (mat, label) = if by < 0 { ("RED_STAINED_GLASS_PANE", format!("&c{by}")) } else { ("LIME_STAINED_GLASS_PANE", format!("&a+{by}")) };
        let to = to.to_owned();
        m.button(slot, gui::icon(mat, &label, &[]), move |ctx| open_pay_amount(&ctx.player, &to, next));
    }
    {
        let to = to.to_owned();
        m.button(22, gui::icon("GOLD_BLOCK", &format!("&6All my money &7({})", money(have as f64)), &[]), move |ctx| open_pay_amount(&ctx.player, &to, have));
    }
    {
        let to = to.to_owned();
        m.button(39, gui::icon("LIME_STAINED_GLASS", &format!("&aPay {} to {to}", money(amount as f64)), &[]), move |ctx| {
            pay(&ctx.player, &to, amount as f64, false);
            open_pay_amount(&ctx.player, &to, amount);
        });
    }
    if p.has_permission("horizonshop.give") {
        let to = to.to_owned();
        m.button(41, gui::icon("EMERALD_BLOCK", &format!("&dModerator: give {} to {to} &7(from the server)", money(amount as f64)), &[]), move |ctx| {
            pay(&ctx.player, &to, amount as f64, true);
            open_pay_amount(&ctx.player, &to, amount);
        });
    }
    m.button(40, gui::icon("ARROW", "&cBack", &[]), |ctx| open_pay_players(&ctx.player));
    m.fill(gui::filler(FILLER));
    m.open(p);
}

/// Sends money: from the payer's balance, or (moderators, `give`) from nothing.
fn pay(p: &Player, to: &str, amount: f64, give: bool) {
    let s = shop();
    if !economy::enabled() {
        return p.send_message(&s.msg("no-economy", &[]));
    }
    if amount <= 0.0 {
        return p.send_message(&s.msg("pay-zero", &[]));
    }
    let Some(target) = horizon_bukkit::get_online_players().into_iter().find(|o| o.get_name().eq_ignore_ascii_case(to)) else {
        return p.send_message(&s.msg("pay-offline", &[("player", to.to_owned())]));
    };
    if give && !p.has_permission("horizonshop.give") {
        return p.send_message("§cYou can't do that.");
    }
    if !give && !economy::withdraw(p.get_name(), amount) {
        return p.send_message(&s.msg("no-money", &[("price", money(amount)), ("balance", balance(p))]));
    }
    if !economy::deposit(target.get_name(), amount) {
        if !give {
            economy::deposit(p.get_name(), amount);
        }
        return p.send_message(&s.msg("no-economy", &[]));
    }
    let pairs = [("player", target.get_name().to_owned()), ("from", p.get_name().to_owned()), ("amount", money(amount))];
    p.send_message(&s.msg("paid", &pairs));
    target.send_message(&s.msg(if give { "given" } else { "got-paid" }, &pairs));
}

// ---- Worth on hover: lines under each item the shop buys, in the copy the client is sent ----
//
// The engine hands every packet going to a player to `pl_send_raw` (on that player's writer thread) and every packet
// coming from one to `pl_receive_raw` (on the game thread), as bytes we may replace. Going out, plain items the shop
// buys get worth lines (lore) in the slot packets; the server's items never change, so selling, stacking and other
// plugins see them as before. Coming in, a creative client's copy has the lines taken off before the server keeps it,
// and a click's slots are marked as unknown so the server sends them again, with fresh lines. Each second the
// player's own slots are sent again when what they'd say changed (a pickup, a market move, a trade).
//
// ponytail: listening makes the engine copy every small packet to hex for the listeners (~20 us each, mostly the
// Java layer's); fine for a small server. Packets over 4 KB (a chest of shulker boxes) pass without lines.

use horizon_bukkit::horizon_mod::{self, Answer, Value};
use horizon_protocol::buf::{Reader, Writer};
use horizon_protocol::item::{Component, Stack, WirePatch, component};
use horizon_protocol::packets::play::{Clientbound, Serverbound, SetPlayerInventory};
use horizon_world::nbt::Tag;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// On each line we add, so we know ours again.
const MARK: &str = "horizonshop:worth";

/// Item names by network id (`minecraft:dirt`), from the engine.
static ITEM_NAMES: OnceLock<Vec<String>> = OnceLock::new();
/// The network ids of the packets that carry a player's stacks: content, slot, cursor, player inventory.
static SLOT_PACKETS: OnceLock<[i32; 4]> = OnceLock::new();
/// Per player (32 hex digits): how many of each item they carry, as of the last second.
static CARRIED: Mutex<Option<HashMap<String, HashMap<String, i64>>>> = Mutex::new(None);
/// Per player: the lines their slots were last sent with, and who got lines from a packet since.
static SHOWN: Mutex<Option<HashMap<String, Vec<(usize, Vec<String>)>>>> = Mutex::new(None);
static TOUCHED: Mutex<Option<HashSet<String>>> = Mutex::new(None);
static ANNOUNCED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static WORKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `$1,234.50`. Made here, not by Essentials: the packet hook runs off the game thread, where it can't be asked.
fn cash(v: f64) -> String {
    let cents = (v.max(0.0) * 100.0).round() as u64;
    let digits = (cents / 100).to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    format!("${out}.{:02}", cents % 100)
}

/// The hover lines for `n` of an item (`minecraft:dirt`) when the player carries `all` of it; None if the shop
/// doesn't buy it.
fn worth_lines(s: &Shop, item: &str, n: i64, all: i64) -> Option<Vec<String>> {
    let mat = item.trim_start_matches("minecraft:").to_ascii_uppercase();
    let it = s.sell_item(&mat)?;
    let mut lines = vec![s.msg("hover-each", &[("each", cash(sell_now(s, it)))])];
    if n > 1 {
        lines.push(s.msg("hover-stack", &[("amount", n.to_string()), ("stack", cash(sell_now_total(s, it, n)))]));
    }
    if all > 0 && all != n {
        lines.push(s.msg("hover-all", &[("all", all.to_string()), ("total", cash(sell_now_total(s, it, all)))]));
    }
    Some(lines)
}

/// The lines as the game's lore: each a text component that isn't italic, marked as ours.
fn lore_tag(lines: &[String]) -> Option<Tag> {
    let comps: Vec<Value> = lines
        .iter()
        .map(|l| {
            let c = horizon_kit::item::text_component(l);
            let extra = match c {
                Value::Array(parts) => parts,
                Value::String(text) => vec![serde_json::json!({ "text": text })],
                other => vec![other],
            };
            serde_json::json!({ "text": "", "italic": false, "insertion": MARK, "extra": extra })
        })
        .collect();
    horizon_world::nbt::parse_snbt(&horizon_bukkit::data::snbt(&Value::Array(comps))).ok().map(|(t, _)| t)
}

/// Adds worth lines to a stack with no components of its own (so no name, lore, enchantments or wear: what the
/// shop buys back). True when it did.
fn add_worth(s: &Shop, st: &mut Stack, names: &[String], carried: &HashMap<String, i64>) -> bool {
    if !st.added.is_empty() || !st.removed.is_empty() {
        return false;
    }
    let Some(item) = usize::try_from(st.item).ok().and_then(|i| names.get(i)) else { return false };
    let n = i64::from(st.count);
    let Some(lines) = worth_lines(s, item, n, carried.get(item).copied().unwrap_or(0)) else { return false };
    let Some(Tag::List(lore)) = lore_tag(&lines) else { return false };
    st.added.push((component::LORE, Component::Lore(lore)));
    st.wire = WirePatch::default();
    true
}

/// Takes our lines off a stack; true if there were any.
fn strip_worth(st: &mut Stack) -> bool {
    let ours = |line: &Tag| matches!(line.get("insertion"), Some(Tag::String(s)) if s.to_string_lossy() == MARK);
    let mut changed = false;
    for (kind, c) in &mut st.added {
        if *kind == component::LORE
            && let Component::Lore(lines) = c
            && lines.iter().any(ours)
        {
            lines.retain(|l| !ours(l));
            changed = true;
        }
    }
    if changed {
        st.added.retain(|(kind, c)| !(*kind == component::LORE && matches!(c, Component::Lore(l) if l.is_empty())));
        st.wire = WirePatch::default();
    }
    changed
}

/// A packet's id from the start of its hex, without reading the rest.
fn hex_id(hex: &str) -> Option<i32> {
    let mut id = 0i32;
    for i in 0..5 {
        let b = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
        id |= i32::from(b & 0x7F) << (7 * i);
        if b & 0x80 == 0 {
            return Some(id);
        }
    }
    None
}

fn unhex(hex: &str) -> Option<Vec<u8>> {
    (0..hex.len() / 2).map(|i| u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()).collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A slot packet going to a player, with worth lines on its plain items; None to send it as it was. A packet our
/// codec wouldn't write back byte for byte is left alone.
pub fn rewrite_out(body: &[u8], ids: &[i32; 4], names: &[String], carried: &HashMap<String, i64>) -> Option<Vec<u8>> {
    let mut r = Reader::new(body);
    let id = r.varint().ok()?;
    if !ids.contains(&id) {
        return None;
    }
    let mut packet = Clientbound::decode(id, r.rest()).ok()?;
    if packet.encode() != body {
        return None;
    }
    let s = shop();
    let mut changed = false;
    let mut add = |st: &mut Option<Stack>| {
        if let Some(st) = st {
            changed |= add_worth(&s, st, names, carried);
        }
    };
    match &mut packet {
        Clientbound::ContainerSetContent(p) => {
            p.items.iter_mut().for_each(&mut add);
            add(&mut p.carried);
        }
        Clientbound::ContainerSetSlot(p) => add(&mut p.item),
        Clientbound::SetCursorItem(p) => add(&mut p.item),
        Clientbound::SetPlayerInventory(p) => add(&mut p.item),
        _ => return None,
    }
    changed.then(|| packet.encode())
}

/// A packet from a player: a creative slot loses our lines, and a click says it doesn't know its slots, so the
/// server sends them back (with lines that fit the new amounts). None to keep it as it was.
pub fn rewrite_in(body: &[u8]) -> Option<Vec<u8>> {
    use horizon_protocol::ids::play::serverbound as ids;
    let mut r = Reader::new(body);
    let id = r.varint().ok()?;
    if id != ids::SET_CREATIVE_MODE_SLOT && id != ids::CONTAINER_CLICK {
        return None;
    }
    let mut packet = Serverbound::decode(id, r.rest()).ok()?;
    if packet.encode() != body {
        return None;
    }
    let changed = match &mut packet {
        Serverbound::SetCreativeModeSlot(p) => p.item.as_mut().is_some_and(strip_worth),
        Serverbound::ContainerClick(p) => {
            let mut any = false;
            for h in p.changed.iter_mut().filter_map(|(_, h)| h.as_mut()).chain(p.carried.as_mut()) {
                h.count = -1;
                any = true;
            }
            any
        }
        _ => false,
    };
    changed.then(|| packet.encode())
}

fn uuid_hex(p: &Player) -> String {
    p.get_unique_id().replace('-', "").to_ascii_lowercase()
}

fn hover_out(event: &Value) -> Answer {
    let (Some(ids), Some(names)) = (SLOT_PACKETS.get(), ITEM_NAMES.get()) else { return Answer::Pass };
    let (Some(hex), Some(uuid)) = (event["body"].as_str(), event["uuid"].as_str()) else { return Answer::Pass };
    if !hex_id(hex).is_some_and(|id| ids.contains(&id)) {
        return Answer::Pass;
    }
    let carried = CARRIED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|c| c.get(uuid).cloned()).unwrap_or_default();
    if let Some(out) = unhex(hex).and_then(|body| rewrite_out(&body, ids, names, &carried)) {
        horizon_mod::query(serde_json::json!({ "op": "packet_edit", "field": "raw", "value": to_hex(&out) }));
        TOUCHED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).insert(uuid.to_owned());
        WORKING.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Answer::Pass
}

fn hover_in(event: &Value) -> Answer {
    if ITEM_NAMES.get().is_none() {
        return Answer::Pass;
    }
    let Some(hex) = event["body"].as_str() else { return Answer::Pass };
    use horizon_protocol::ids::play::serverbound as ids;
    let Some(id) = hex_id(hex).filter(|id| *id == ids::SET_CREATIVE_MODE_SLOT || *id == ids::CONTAINER_CLICK) else {
        return Answer::Pass;
    };
    // A creative client changes its own slots and the server sends nothing back: its new stacks have no lines
    // until the timer sends the slots again, so it must.
    if id == ids::SET_CREATIVE_MODE_SLOT
        && let Some(uuid) = event["uuid"].as_str()
    {
        TOUCHED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).insert(uuid.to_owned());
    }
    if let Some(out) = unhex(hex).and_then(|body| rewrite_in(&body)) {
        horizon_mod::query(serde_json::json!({ "op": "packet_edit", "field": "raw", "value": to_hex(&out) }));
    }
    Answer::Pass
}

/// The engine's item and packet tables, once a player is on (the item table comes from the running server).
fn hover_tables() -> bool {
    if ITEM_NAMES.get().is_none() {
        let names: Vec<String> = horizon_mod::query(serde_json::json!({ "op": "paper_registry", "registry": "item" }))
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        // Network ids must be the game's own (air is 0, stone 1), or dirt could show another item's price.
        if names.first().map(String::as_str) != Some("minecraft:air") || names.get(1).map(String::as_str) != Some("minecraft:stone") {
            return false;
        }
        let _ = ITEM_NAMES.set(names);
    }
    if SLOT_PACKETS.get().is_none() {
        const NAMES: [&str; 4] = ["ContainerSetContent", "ContainerSetSlot", "SetCursorItem", "SetPlayerInventory"];
        let answer = horizon_mod::query(serde_json::json!({ "op": "packet_ids", "names": NAMES }));
        let ids: Vec<i32> = NAMES.iter().filter_map(|n| answer[*n].as_i64().map(|i| i as i32)).collect();
        // Our packet code must number them as the engine does, or we'd read one packet as another.
        use horizon_protocol::ids::play::clientbound as ours;
        let mine = [ours::CONTAINER_SET_CONTENT, ours::CONTAINER_SET_SLOT, ours::SET_CURSOR_ITEM, ours::SET_PLAYER_INVENTORY];
        if ids != mine {
            if !ANNOUNCED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                app::warn(&format!("Worth on hover is off: this HorizonX numbers its packets {ids:?}, the shop {mine:?}."));
            }
            return false;
        }
        let _ = SLOT_PACKETS.set(mine);
    }
    true
}

/// Each second: what each player carries (for "all you carry"), and their own slots sent again with fresh lines
/// when those would read differently, or when a packet got lines since.
fn refresh_hover() {
    let players = horizon_bukkit::get_online_players();
    if players.is_empty() || !hover_tables() {
        return;
    }
    if WORKING.load(std::sync::atomic::Ordering::Relaxed) && !ANNOUNCED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        app::info("Worth on hover is working: items now show what they sell for.");
    }
    let Some(names) = ITEM_NAMES.get() else { return };
    let s = shop();
    let online: HashSet<String> = players.iter().map(uuid_hex).collect();
    for p in &players {
        let uuid = uuid_hex(p);
        // (slot, item, amount) of each stack with no components: the ones that get lines.
        let plain: Vec<(usize, String, i64)> = p
            .get_inventory()
            .get_storage_contents()
            .iter()
            .enumerate()
            .filter_map(|(slot, st)| {
                let st = st.as_ref()?;
                let json = st.to_json();
                json["components"].as_object().is_some_and(|c| c.is_empty()).then_some(())?;
                Some((slot, json["item"].as_str()?.to_owned(), i64::from(st.get_amount())))
            })
            .collect();
        let mut carried: HashMap<String, i64> = HashMap::new();
        for (_, item, n) in &plain {
            *carried.entry(item.clone()).or_default() += n;
        }
        let shown: Vec<(usize, Vec<String>)> =
            plain.iter().filter_map(|(slot, item, n)| Some((*slot, worth_lines(&s, item, *n, carried[item])?))).collect();
        CARRIED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(uuid.clone(), carried.clone());
        let touched = TOUCHED.lock().unwrap_or_else(|e| e.into_inner()).as_mut().is_some_and(|t| t.remove(&uuid));
        {
            let mut all = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
            let all = all.get_or_insert_with(HashMap::new);
            if !touched && all.get(&uuid) == Some(&shown) {
                continue;
            }
            all.insert(uuid.clone(), shown);
        }
        // Each slot again as SetPlayerInventory (no menu state to get out of step), lines already on, so the hook
        // leaves it be.
        let mut hex = String::new();
        for (slot, item, n) in &plain {
            let Some(id) = names.iter().position(|x| x == item) else { continue };
            let mut st = Stack::new(id as i32, *n as i32);
            if !add_worth(&s, &mut st, names, &carried) {
                continue;
            }
            let body = Clientbound::SetPlayerInventory(SetPlayerInventory { slot: *slot as i32, item: Some(st) }).encode();
            let mut w = Writer::new();
            w.varint(body.len() as i32);
            hex.push_str(&to_hex(&w.data));
            hex.push_str(&to_hex(&body));
        }
        if !hex.is_empty() {
            horizon_mod::query(serde_json::json!({ "op": "packets_send", "packets": [{ "raw": hex, "to": [uuid] }] }));
        }
    }
    if let Some(m) = CARRIED.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        m.retain(|k, _| online.contains(k));
    }
    if let Some(m) = SHOWN.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        m.retain(|k, _| online.contains(k));
    }
}

/// Where the sell chest's green button sits; it's never paid for or handed out.
const SELL_BUTTON: usize = 49;

fn open_sell_chest(p: &Player) {
    let s = shop();
    let mut m = Menu::new(6, &format!("{} &7| &a{}", s.settings.sell_title, balance(p)));
    m.tag("horizonshop:sell");
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
            if !sender.has_permission("horizonshop.admin") {
                sender.send_message("§cYou can't do that.");
                return true;
            }
            gui::close_all();
            let s = load();
            let items: usize = s.pages.iter().map(|(_, p)| p.items.len()).sum();
            sender.send_message(&s.msg("reloaded", &[("shops", s.pages.len().to_string()), ("items", items.to_string())]));
        }
        Some("pay") => match sender.as_player() {
            Some(p) => open_pay_players(p),
            None => sender.send_message("Only players can pay."),
        },
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
        let mut out = vec!["sell".to_owned(), "pay".to_owned()];
        if s.has_permission("horizonshop.admin") {
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
    // Worth on hover: the engine's packet hooks (as ProtocolLib and PacketEvents use them on HorizonX).
    horizon_mod::Host.on("pl_send_raw", hover_out);
    horizon_mod::Host.on("pl_receive_raw", hover_in);
    horizon_mod::query(serde_json::json!({ "op": "packet_listen", "name": "raw:out" }));
    horizon_mod::query(serde_json::json!({ "op": "packet_listen", "name": "raw:in" }));
    plugin.run_task_timer(40, 20, refresh_hover);
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
    plugin.info(&format!("HorizonShop (Rust, after Genius Shop) ready: {} shops, {items} items", s.pages.len()));
}

/// The "Menu" button in the Escape menu is a data pack (a dialog in the `pause_screen_additions` tag): written
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

    /// Worth on hover, through the game's own packet code: dirt in a content packet gets its lines (one, the
    /// stack, all carried), a named stack doesn't; a creative client sending it back gets exactly the plain stack;
    /// a click's slots come out unknown so the server sends them again.
    #[test]
    fn hover_lines_go_on_plain_items_and_come_off_in_creative() {
        use horizon_protocol::ids::play::clientbound as cb;
        use horizon_protocol::item::Hashed;
        use horizon_protocol::packets::play::{ContainerClick, ContainerSetContent, SetCreativeModeSlot};
        fn texts(t: &Tag, out: &mut String) {
            match t {
                Tag::String(s) => out.push_str(&s.to_string_lossy()),
                Tag::Compound(_) => {
                    for key in ["text", "extra"] {
                        if let Some(x) = t.get(key) {
                            texts(x, out);
                        }
                    }
                }
                Tag::List(l) => l.iter().for_each(|x| texts(x, out)),
                _ => {}
            }
        }
        assert_eq!(cash(1234.5), "$1,234.50");
        assert_eq!(cash(0.1), "$0.10");
        let names: Vec<String> = ["minecraft:air", "minecraft:stone", "minecraft:dirt", "minecraft:barrier"].map(String::from).to_vec();
        let ids = [cb::CONTAINER_SET_CONTENT, cb::CONTAINER_SET_SLOT, cb::SET_CURSOR_ITEM, cb::SET_PLAYER_INVENTORY];
        let mut named = Stack::new(2, 5);
        named.added.push((component::CUSTOM_NAME, Component::Text(horizon_protocol::text::literal("Mine"))));
        let packet = Clientbound::ContainerSetContent(ContainerSetContent {
            container: 0,
            state: 3,
            items: vec![None, Some(Stack::new(2, 64)), Some(named.clone()), Some(Stack::new(3, 1))],
            carried: None,
        });
        let carried: HashMap<String, i64> = [("minecraft:dirt".to_owned(), 128)].into();
        let out = rewrite_out(&packet.encode(), &ids, &names, &carried).expect("dirt gets lines");
        let mut r = Reader::new(&out);
        let id = r.varint().unwrap();
        let Clientbound::ContainerSetContent(p) = Clientbound::decode(id, r.rest()).unwrap() else { panic!("not content") };
        let dirt = p.items[1].clone().unwrap();
        let Some(Component::Lore(lines)) = dirt.component(component::LORE) else { panic!("no lore on dirt") };
        let shown: Vec<String> = lines.iter().map(|l| { let mut s = String::new(); texts(l, &mut s); s }).collect();
        println!("{shown:?}");
        assert_eq!(shown, ["Worth: $0.10 each", "This stack (64): $6.40", "All you carry (128): $12.80"]);
        assert_eq!(p.items[2], Some(named), "a named stack is left alone");
        assert_eq!(p.items[3], Some(Stack::new(3, 1)), "the shop doesn't buy barriers");
        // Lines already on: nothing to do (what the timer sends passes the hook as it is).
        assert!(rewrite_out(&out, &ids, &names, &carried).is_none());
        // A creative client sends the lined dirt back: the server gets plain dirt.
        let creative = Serverbound::SetCreativeModeSlot(SetCreativeModeSlot { slot: 36, item: Some(dirt) });
        let back = rewrite_in(&creative.encode()).expect("lines come off");
        let mut r = Reader::new(&back);
        let id = r.varint().unwrap();
        let Serverbound::SetCreativeModeSlot(p) = Serverbound::decode(id, r.rest()).unwrap() else { panic!("not creative") };
        assert_eq!(p.item, Some(Stack::new(2, 64)));
        // A click: every slot it names comes out unknown.
        let click = Serverbound::ContainerClick(ContainerClick {
            container: 0,
            state: 3,
            slot: 36,
            button: 1,
            input: 0,
            changed: vec![(36, Some(Hashed::of(&Stack::new(2, 32)))), (37, None)],
            carried: Some(Hashed::of(&Stack::new(2, 32))),
        });
        let back = rewrite_in(&click.encode()).expect("slots marked unknown");
        let mut r = Reader::new(&back);
        let id = r.varint().unwrap();
        let Serverbound::ContainerClick(p) = Serverbound::decode(id, r.rest()).unwrap() else { panic!("not a click") };
        assert_eq!(p.changed[0].1.as_ref().map(|h| h.count), Some(-1));
        assert_eq!(p.changed[1].1, None);
        assert_eq!(p.carried.map(|h| h.count), Some(-1));
        assert_eq!((p.slot, p.button, p.state), (36, 1, 3));
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
