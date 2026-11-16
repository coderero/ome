use egui::Color32;
use egui_dock::{DockState, NodeIndex};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tab {
    Monitor,
    Trade,
    Charts,
    MarketWatch,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Monitor, Tab::Trade, Tab::Charts, Tab::MarketWatch];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Monitor => "Monitor",
            Tab::Trade => "Trade",
            Tab::Charts => "Charts",
            Tab::MarketWatch => "MarketWatch",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Link {
    Red,
    Green,
    Blue,
    Yellow,
    Purple,
}

impl Link {
    pub const ALL: [Link; 5] = [Link::Red, Link::Green, Link::Blue, Link::Yellow, Link::Purple];

    pub fn color(self) -> Color32 {
        match self {
            Link::Red => Color32::from_rgb(0xe0, 0x40, 0x40),
            Link::Green => Color32::from_rgb(0x3c, 0xc0, 0x4c),
            Link::Blue => Color32::from_rgb(0x40, 0x90, 0xf0),
            Link::Yellow => Color32::from_rgb(0xf0, 0xc8, 0x30),
            Link::Purple => Color32::from_rgb(0xb0, 0x70, 0xf0),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Link::Red => "Red",
            Link::Green => "Green",
            Link::Blue => "Blue",
            Link::Yellow => "Yellow",
            Link::Purple => "Purple",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Activity,
    Statement,
    AllProducts,
    ActiveTrader,
    Chart,
    QuoteBoard,
    News,
    LevelTwo,
    TimeSales,
    Watchlist,
}

impl Kind {
    pub const ALL: [Kind; 10] = [
        Kind::Activity,
        Kind::Statement,
        Kind::AllProducts,
        Kind::ActiveTrader,
        Kind::Chart,
        Kind::LevelTwo,
        Kind::TimeSales,
        Kind::Watchlist,
        Kind::QuoteBoard,
        Kind::News,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Kind::Activity => "Activity and Positions",
            Kind::Statement => "Account Statement",
            Kind::AllProducts => "All Products",
            Kind::ActiveTrader => "Active Trader",
            Kind::Chart => "Chart",
            Kind::QuoteBoard => "Quote Board",
            Kind::News => "News",
            Kind::LevelTwo => "Level II",
            Kind::TimeSales => "Time & Sales",
            Kind::Watchlist => "Watchlist",
        }
    }

    pub fn follows_symbol(self) -> bool {
        matches!(self, Kind::AllProducts | Kind::ActiveTrader | Kind::Chart | Kind::LevelTwo | Kind::TimeSales)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gadget {
    pub id: u32,
    pub kind: Kind,
    pub link: Link,
    pub interval: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    AccountInfo,
    Watchlist,
    Quote,
    LiveNews,
}

impl Side {
    pub const ALL: [Side; 4] = [Side::AccountInfo, Side::Watchlist, Side::Quote, Side::LiveNews];

    pub fn title(self) -> &'static str {
        match self {
            Side::AccountInfo => "Account Info",
            Side::Watchlist => "Watchlist",
            Side::Quote => "Quote Details",
            Side::LiveNews => "Live News",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct View {
    pub tab: Tab,
    pub docks: Vec<DockState<Gadget>>,
    pub sidebar: Vec<Side>,
    pub sidebar_open: bool,
    pub collapsed: BTreeSet<String>,
    pub hidden: BTreeSet<String>,
    pub detached: Vec<Gadget>,
    pub next_id: u32,
}

pub const COLLAPSED_AT_FIRST: [&str; 2] = ["Cash Balance", "Profits and Losses"];
pub const INTERVALS: [u64; 4] = [1_000, 15_000, 60_000, 300_000];

impl Default for View {
    fn default() -> Self {
        let mut view = Self {
            tab: Tab::Monitor,
            docks: Vec::new(),
            sidebar: vec![Side::AccountInfo, Side::Watchlist, Side::LiveNews],
            sidebar_open: true,
            collapsed: COLLAPSED_AT_FIRST.iter().map(|s| s.to_string()).collect(),
            hidden: BTreeSet::new(),
            detached: Vec::new(),
            next_id: 1,
        };
        view.docks = Tab::ALL.iter().map(|t| view.preset(*t)).collect();
        view
    }
}

impl View {
    pub fn gadget(&mut self, kind: Kind) -> Gadget {
        let id = self.next_id;
        self.next_id += 1;
        Gadget { id, kind, link: Link::Red, interval: 1_000 }
    }

    pub fn preset(&mut self, tab: Tab) -> DockState<Gadget> {
        match tab {
            Tab::Monitor => DockState::new(vec![self.gadget(Kind::Activity), self.gadget(Kind::Statement)]),
            Tab::Trade => {
                let mut dock = DockState::new(vec![self.gadget(Kind::AllProducts), self.gadget(Kind::ActiveTrader)]);
                let chart = self.gadget(Kind::Chart);
                dock.main_surface_mut().split_right(NodeIndex::root(), 0.5, vec![chart]);
                dock
            }
            Tab::Charts => self.chart_grid(1),
            Tab::MarketWatch => DockState::new(vec![self.gadget(Kind::QuoteBoard), self.gadget(Kind::News)]),
        }
    }

    pub fn chart_grid(&mut self, cells: usize) -> DockState<Gadget> {
        let mut charts: Vec<Gadget> = (0..cells.max(1)).map(|_| self.gadget(Kind::Chart)).collect();
        for (chart, interval) in charts.iter_mut().zip(INTERVALS) {
            chart.interval = interval;
        }
        let mut dock = DockState::new(vec![charts[0]]);
        let s = dock.main_surface_mut();
        match cells {
            2 => {
                s.split_right(NodeIndex::root(), 0.5, vec![charts[1]]);
            }
            4 => {
                let [left, right] = s.split_right(NodeIndex::root(), 0.5, vec![charts[1]]);
                s.split_below(left, 0.5, vec![charts[2]]);
                s.split_below(right, 0.5, vec![charts[3]]);
            }
            _ => {}
        }
        dock
    }

    pub fn trading_layout(&mut self, layout: Trading) -> DockState<Gadget> {
        match layout {
            Trading::LadderChart => {
                let mut dock = DockState::new(vec![self.gadget(Kind::ActiveTrader)]);
                let chart = self.gadget(Kind::Chart);
                dock.main_surface_mut().split_right(NodeIndex::root(), 0.45, vec![chart]);
                dock
            }
            Trading::WatchChartLadder => {
                let mut dock = DockState::new(vec![self.gadget(Kind::Chart)]);
                let watch = self.gadget(Kind::Watchlist);
                let ladder = self.gadget(Kind::ActiveTrader);
                let s = dock.main_surface_mut();
                let [chart, _] = s.split_left(NodeIndex::root(), 0.22, vec![watch]);
                s.split_right(chart, 0.55, vec![ladder]);
                dock
            }
            Trading::LadderChartTape => {
                let mut dock = DockState::new(vec![self.gadget(Kind::ActiveTrader)]);
                let chart = self.gadget(Kind::Chart);
                let book = vec![self.gadget(Kind::LevelTwo), self.gadget(Kind::TimeSales)];
                let s = dock.main_surface_mut();
                let [_, right] = s.split_right(NodeIndex::root(), 0.42, vec![chart]);
                s.split_below(right, 0.62, book);
                dock
            }
        }
    }

    pub fn set_trading_layout(&mut self, layout: Trading) {
        self.tab = Tab::Trade;
        let dock = self.trading_layout(layout);
        *self.dock() = dock;
    }

    pub fn dock(&mut self) -> &mut DockState<Gadget> {
        let i = self.tab as usize;
        while self.docks.len() <= i {
            let tab = Tab::ALL[self.docks.len()];
            let preset = self.preset(tab);
            self.docks.push(preset);
        }
        &mut self.docks[i]
    }

    pub fn add(&mut self, kind: Kind) {
        let gadget = self.gadget(kind);
        self.dock().push_to_focused_leaf(gadget);
    }

    pub fn focus(&mut self, tab: Tab, kind: Kind) {
        self.tab = tab;
        let dock = self.dock();
        match dock.find_tab_from(|g| g.kind == kind) {
            Some(path) => {
                let _ = dock.set_active_tab(path);
                dock.set_focused_node_and_surface(egui_dock::NodePath { surface: path.surface, node: path.node });
            }
            None => self.add(kind),
        }
    }

    pub fn reset_tab(&mut self) {
        let preset = self.preset(self.tab);
        *self.dock() = preset;
    }

    pub fn set_chart_grid(&mut self, cells: usize) {
        self.tab = Tab::Charts;
        let grid = self.chart_grid(cells);
        *self.dock() = grid;
    }

    pub fn detach(&mut self, id: u32) -> bool {
        for dock in &mut self.docks {
            if let Some(path) = dock.find_tab_from(|g| g.id == id)
                && let Some(gadget) = dock.remove_tab(path)
            {
                self.detached.push(gadget);
                return true;
            }
        }
        false
    }

    pub fn reattach(&mut self, id: u32) {
        if let Some(i) = self.detached.iter().position(|g| g.id == id) {
            let gadget = self.detached.remove(i);
            self.dock().push_to_focused_leaf(gadget);
        }
    }

    pub fn set_link(&mut self, id: u32, link: Link) {
        for dock in &mut self.docks {
            for (_, g) in dock.iter_all_tabs_mut() {
                if g.id == id {
                    g.link = link;
                }
            }
        }
        for g in &mut self.detached {
            if g.id == id {
                g.link = link;
            }
        }
    }

    pub fn move_side(&mut self, side: Side, delta: i64) {
        if let Some(i) = self.sidebar.iter().position(|s| *s == side) {
            let j = (i as i64 + delta).clamp(0, self.sidebar.len() as i64 - 1) as usize;
            self.sidebar.swap(i, j);
        }
    }

    pub fn is_open(&self, section: &str) -> bool {
        !self.collapsed.contains(section)
    }

    pub fn set_open(&mut self, section: &str, open: bool) {
        if open {
            self.collapsed.remove(section);
        } else {
            self.collapsed.insert(section.to_string());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trading {
    LadderChart,
    WatchChartLadder,
    LadderChartTape,
}

impl Trading {
    pub const ALL: [Trading; 3] = [Trading::LadderChart, Trading::WatchChartLadder, Trading::LadderChartTape];

    pub fn label(self) -> &'static str {
        match self {
            Trading::LadderChart => "Ladder beside chart",
            Trading::WatchChartLadder => "Watchlist, chart and ladder",
            Trading::LadderChartTape => "Ladder, chart, Level II and T&S",
        }
    }
}

pub type Saved = BTreeMap<String, View>;

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(view: &View, tab: Tab) -> Vec<Kind> {
        view.docks[tab as usize].iter_all_tabs().map(|(_, g)| g.kind).collect()
    }

    #[test]
    fn every_tab_starts_with_its_gadgets_and_unique_ids() {
        let view = View::default();
        assert_eq!(kinds(&view, Tab::Monitor), [Kind::Activity, Kind::Statement]);
        assert_eq!(kinds(&view, Tab::Trade), [Kind::AllProducts, Kind::ActiveTrader, Kind::Chart], "the chart sits beside order entry and the ladder");
        assert_eq!(kinds(&view, Tab::Charts), [Kind::Chart]);
        let all: Vec<u32> = view.docks.iter().flat_map(|d| d.iter_all_tabs().map(|(_, g)| g.id)).collect();
        let mut ids = all.clone();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len());
        assert!(!view.is_open("Cash Balance") && view.is_open("Position Statement"));
    }

    #[test]
    fn gadgets_are_added_detached_reattached_relinked_and_grids_built() {
        let mut view = View::default();
        view.add(Kind::Chart);
        let chart = view.docks[0].iter_all_tabs().find(|(_, g)| g.kind == Kind::Chart).map(|(_, g)| g.id).unwrap();
        assert!(view.detach(chart));
        assert!(!kinds(&view, Tab::Monitor).contains(&Kind::Chart));
        assert_eq!(view.detached.len(), 1);
        view.set_link(chart, Link::Blue);
        assert_eq!(view.detached[0].link, Link::Blue);
        view.tab = Tab::Trade;
        view.reattach(chart);
        assert!(view.detached.is_empty() && kinds(&view, Tab::Trade).contains(&Kind::Chart));

        view.set_chart_grid(4);
        assert_eq!(view.tab, Tab::Charts);
        let intervals: Vec<u64> = view.docks[Tab::Charts as usize].iter_all_tabs().map(|(_, g)| g.interval).collect();
        assert_eq!(intervals.len(), 4);
        assert!(intervals.contains(&1_000) && intervals.contains(&300_000), "each cell gets its own timeframe");
        view.reset_tab();
        assert_eq!(kinds(&view, Tab::Charts), [Kind::Chart]);

        view.set_trading_layout(Trading::WatchChartLadder);
        assert_eq!(view.tab, Tab::Trade);
        let mut trade = kinds(&view, Tab::Trade);
        trade.sort_by_key(|k| k.title());
        assert_eq!(trade, [Kind::ActiveTrader, Kind::Chart, Kind::Watchlist]);
        view.set_trading_layout(Trading::LadderChartTape);
        assert_eq!(kinds(&view, Tab::Trade).len(), 4);

        view.move_side(Side::LiveNews, -5);
        assert_eq!(view.sidebar[0], Side::LiveNews);
    }

    #[test]
    fn views_survive_the_ron_storage_eframe_uses() {
        let mut saved = Saved::new();
        let mut view = View::default();
        view.set_chart_grid(2);
        view.hidden.insert("positions:P/L %".into());
        saved.insert("alice".into(), view);
        let back: Saved = ron::from_str(&ron::to_string(&saved).unwrap()).unwrap();
        assert_eq!(back["alice"].tab, Tab::Charts);
        assert_eq!(kinds(&back["alice"], Tab::Charts), [Kind::Chart, Kind::Chart]);
        assert!(back["alice"].hidden.contains("positions:P/L %"));
    }
}
