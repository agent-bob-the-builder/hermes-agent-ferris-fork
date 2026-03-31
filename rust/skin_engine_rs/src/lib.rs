use once_cell::sync::Lazy;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde::{Deserialize, Serialize};
use serde_yaml::Value as YamlValue;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[pyclass]
pub struct SkinConfig {
    name: String,
    description: String,
    #[serde(default)]
    colors: HashMap<String, String>,
    #[serde(default)]
    spinner: HashMap<String, YamlValue>,
    #[serde(default)]
    branding: HashMap<String, String>,
    #[serde(default)]
    tool_prefix: String,
    #[serde(default)]
    tool_emojis: HashMap<String, String>,
    #[serde(default)]
    banner_logo: String,
    #[serde(default)]
    banner_hero: String,
}

#[pymethods]
impl SkinConfig {
    #[getter]
    fn name(&self) -> String { self.name.clone() }
    #[getter]
    fn description(&self) -> String { self.description.clone() }
    #[getter]
    fn colors(&self) -> HashMap<String, String> { self.colors.clone() }
    #[getter]
    fn branding(&self) -> HashMap<String, String> { self.branding.clone() }
    #[getter]
    fn tool_prefix(&self) -> String { self.tool_prefix.clone() }
    #[getter]
    fn tool_emojis(&self) -> HashMap<String, String> { self.tool_emojis.clone() }
    #[getter]
    fn banner_logo(&self) -> String { self.banner_logo.clone() }
    #[getter]
    fn banner_hero(&self) -> String { self.banner_hero.clone() }
    fn get_color(&self, key: &str, fallback: &str) -> String {
        self.colors.get(key).cloned().unwrap_or_else(|| fallback.to_string())
    }
    fn get_branding_key(&self, key: &str, fallback: &str) -> String {
        self.branding.get(key).cloned().unwrap_or_else(|| fallback.to_string())
    }
    fn get_spinner_list(&self, key: &str) -> Vec<String> {
        self.spinner.get(key).and_then(|v: &YamlValue| v.as_sequence()).map(|seq: &serde_yaml::Sequence| {
            seq.iter().filter_map(|item: &YamlValue| item.as_str().map(String::from)).collect()
        }).unwrap_or_default()
    }
    fn get_spinner_wings(&self) -> Vec<(String, String)> {
        self.spinner.get("wings").and_then(|v: &YamlValue| v.as_sequence()).map(|seq: &serde_yaml::Sequence| {
            seq.iter().filter_map(|item: &YamlValue| {
                item.as_sequence().and_then(|pair: &serde_yaml::Sequence| {
                    if pair.len() == 2 {
                        Some((pair[0].as_str().unwrap_or("").to_string(), pair[1].as_str().unwrap_or("").to_string()))
                    } else { None }
                })
            }).collect()
        }).unwrap_or_default()
    }
}

fn sv(s: &str) -> YamlValue { YamlValue::String(s.to_string()) }
fn vi(v: Vec<&str>) -> YamlValue { YamlValue::Sequence(v.into_iter().map(|s| YamlValue::String(s.to_string())).collect()) }
fn vv(v: Vec<Vec<&str>>) -> YamlValue { YamlValue::Sequence(v.into_iter().map(|pair| YamlValue::Sequence(pair.into_iter().map(|s| YamlValue::String(s.to_string())).collect())).collect()) }
fn vd() -> HashMap<String, YamlValue> { HashMap::new() }

macro_rules! skin_data {
    ($name:expr, $desc:expr, $colors:expr, $spinner:expr, $branding:expr, $tp:expr, $logo:expr, $hero:expr) => {{
        let mut d = HashMap::new();
        d.insert("name".to_string(), sv($name));
        d.insert("description".to_string(), sv($desc));
        d.insert("colors".to_string(), serde_yaml::to_value($colors).unwrap());
        d.insert("spinner".to_string(), serde_yaml::to_value(&$spinner).unwrap());
        d.insert("branding".to_string(), serde_yaml::to_value(&$branding).unwrap());
        d.insert("tool_prefix".to_string(), sv($tp));
        if !$logo.is_empty() { d.insert("banner_logo".to_string(), sv($logo)); }
        if !$hero.is_empty() { d.insert("banner_hero".to_string(), sv($hero)); }
        d
    }};
    ($name:expr, $desc:expr, $colors:expr, $spinner:expr, $branding:expr, $tp:expr) => {{
        let mut d = HashMap::new();
        d.insert("name".to_string(), sv($name));
        d.insert("description".to_string(), sv($desc));
        d.insert("colors".to_string(), serde_yaml::to_value($colors).unwrap());
        d.insert("spinner".to_string(), serde_yaml::to_value(&$spinner).unwrap());
        d.insert("branding".to_string(), serde_yaml::to_value(&$branding).unwrap());
        d.insert("tool_prefix".to_string(), sv($tp));
        d
    }};
}

type SkinData = HashMap<String, YamlValue>;

fn make_builtin_skins() -> HashMap<&'static str, SkinData> {
    let mut skins = HashMap::new();

    // --- default ---
    {
        let mut c = HashMap::new();
        c.insert("banner_border", "#CD7F32"); c.insert("banner_title", "#FFD700");
        c.insert("banner_accent", "#FFBF00"); c.insert("banner_dim", "#B8860B");
        c.insert("banner_text", "#FFF8DC"); c.insert("ui_accent", "#FFBF00");
        c.insert("ui_label", "#4dd0e1"); c.insert("ui_ok", "#4caf50");
        c.insert("ui_error", "#ef5350"); c.insert("ui_warn", "#ffa726");
        c.insert("prompt", "#FFF8DC"); c.insert("input_rule", "#CD7F32");
        c.insert("response_border", "#FFD700"); c.insert("session_label", "#DAA520");
        c.insert("session_border", "#8B8682");
        let mut b = HashMap::new();
        b.insert("agent_name", "Hermes Agent"); b.insert("welcome", "Welcome to Hermes Agent! Type your message or /help for commands.");
        b.insert("goodbye", "Goodbye! ⚕"); b.insert("response_label", " ⚕ Hermes ");
        b.insert("prompt_symbol", "❯ "); b.insert("help_header", "(^_^)? Available Commands");
        skins.insert("default", skin_data!("default", "Classic Hermes — gold and kawaii", c, vd(), b, "┊"));
    }

    // --- ares ---
    {
        let mut c = HashMap::new();
        c.insert("banner_border", "#9F1C1C"); c.insert("banner_title", "#C7A96B");
        c.insert("banner_accent", "#DD4A3A"); c.insert("banner_dim", "#6B1717");
        c.insert("banner_text", "#F1E6CF"); c.insert("ui_accent", "#DD4A3A");
        c.insert("ui_label", "#C7A96B"); c.insert("ui_ok", "#4caf50");
        c.insert("ui_error", "#ef5350"); c.insert("ui_warn", "#ffa726");
        c.insert("prompt", "#F1E6CF"); c.insert("input_rule", "#9F1C1C");
        c.insert("response_border", "#C7A96B"); c.insert("session_label", "#C7A96B");
        c.insert("session_border", "#6E584B");
        let mut sp = HashMap::new();
        sp.insert("waiting_faces", vi(vec!["(⚔)", "(⛨)", "(▲)", "(<>)", "(/)"]));
        sp.insert("thinking_faces", vi(vec!["(⚔)", "(⛨)", "(▲)", "(⌁)", "(<>)"]));
        sp.insert("thinking_verbs", vi(vec!["forging", "marching", "sizing the field", "holding the line", "hammering plans", "tempering steel", "plotting impact", "raising the shield"]));
        sp.insert("wings", vv(vec![vec!["⟪⚔", "⚔⟫"], vec!["⟪▲", "▲⟫"], vec!["⟪╸", "╺⟫"], vec!["⟪⛨", "⛨⟫"]]));
        let mut b = HashMap::new();
        b.insert("agent_name", "Ares Agent"); b.insert("welcome", "Welcome to Ares Agent! Type your message or /help for commands.");
        b.insert("goodbye", "Farewell, warrior! ⚔"); b.insert("response_label", " ⚔ Ares ");
        b.insert("prompt_symbol", "⚔ ❯ "); b.insert("help_header", "(⚔) Available Commands");
        let logo = "[bold #A3261F] █████╗ ██████╗ ███████╗███████╗       █████╗  ██████╗ ███████╗███╗   ██╗████████╗[/]\n[bold #B73122]██╔══██╗██╔══██╗██╔════╝██╔════╝      ██╔══██╗██╔════╝ ██╔════╝████╗  ██║╚══██╔══╝[/]\n[#C93C24]███████║██████╔╝█████╗  ███████╗█████╗███████║██║  ███╗█████╗  ██╔██╗ ██║   ██║[/]\n[#D84A28]██╔══██║██╔══██╗██╔══╝  ╚════██║╚════╝██╔══██║██║   ██║██╔══╝  ██║╚██╗██║   ██║[/]\n[#E15A2D]██║  ██║██║  ██║███████╗███████║      ██║  ██║╚██████╔╝███████╗██║ ╚████║   ██║[/]\n[#EB6C32]╚═╝  ╚═╝╚═╝  ╚═╝╚══════╝╚══════╝      ╚═╝  ╚═╝ ╚═════╝ ╚══════╝╚═╝  ╚═══╝   ╚═╝[/]";
        let hero = "[#9F1C1C]⠀⠀⠀⠀━━━━━━━━━━━━━━━━━━━━━━[/]\n[#9F1C1C]⡠⠊⠉⠉⠉⠉⠉⠑⠊⠔⣔[/]\n[#C7A96B]⣀⠤⠒⠒⠒⠒⠒⠤⣀[/]\n[#C7A96B]⢀⣠⣤⣤⣤⣤⣤⣄⡀[/]\n[#DD4A3A]⢸⣿⣿⣿⣿⣿⣿⣿⡇[/]\n[#DD4A3A]⢸⣿⣿⣿⣿⣿⣿⣿⡇[/]\n[#9F1C1C]⢸⣿⣿⣿⣿⣿⣿⣿⡇[/]\n[#9F1C1C]⢸⣿⠋  ⠙⣿⡇[/]\n[#6B1717]⢸⣿      �⣿⡇[/]\n[#6B1717]⠸⣿    ⣸⠇[/]\n[#C7A96B]  ⠙⠒⠒⠙[/]\n[#C7A96B]  ⠒⠒⠒⠒[/]\n[#DD4A3A]  ⚔[/]\n[dim #6B1717]war god online[/]";
        skins.insert("ares", skin_data!("ares", "War-god theme — crimson and bronze", c, sp, b, "╎", logo, hero));
    }

    // --- mono ---
    {
        let mut c = HashMap::new();
        c.insert("banner_border", "#555555"); c.insert("banner_title", "#e6edf3");
        c.insert("banner_accent", "#aaaaaa"); c.insert("banner_dim", "#444444");
        c.insert("banner_text", "#c9d1d9"); c.insert("ui_accent", "#aaaaaa");
        c.insert("ui_label", "#888888"); c.insert("ui_ok", "#888888");
        c.insert("ui_error", "#cccccc"); c.insert("ui_warn", "#999999");
        c.insert("prompt", "#c9d1d9"); c.insert("input_rule", "#444444");
        c.insert("response_border", "#aaaaaa"); c.insert("session_label", "#888888");
        c.insert("session_border", "#555555");
        skins.insert("mono", skin_data!("mono", "Monochrome — clean grayscale", c, vd(), vd(), "┊"));
    }

    // --- slate ---
    {
        let mut c = HashMap::new();
        c.insert("banner_border", "#4169e1"); c.insert("banner_title", "#7eb8f6");
        c.insert("banner_accent", "#8EA8FF"); c.insert("banner_dim", "#4b5563");
        c.insert("banner_text", "#c9d1d9"); c.insert("ui_accent", "#7eb8f6");
        c.insert("ui_label", "#8EA8FF"); c.insert("ui_ok", "#63D0A6");
        c.insert("ui_error", "#F7A072"); c.insert("ui_warn", "#e6a855");
        c.insert("prompt", "#c9d1d9"); c.insert("input_rule", "#4169e1");
        c.insert("response_border", "#7eb8f6"); c.insert("session_label", "#7eb8f6");
        c.insert("session_border", "#4b5563");
        skins.insert("slate", skin_data!("slate", "Cool blue — developer-focused", c, vd(), vd(), "┊"));
    }

    // --- poseidon ---
    {
        let mut c = HashMap::new();
        c.insert("banner_border", "#2A6FB9"); c.insert("banner_title", "#A9DFFF");
        c.insert("banner_accent", "#5DB8F5"); c.insert("banner_dim", "#153C73");
        c.insert("banner_text", "#EAF7FF"); c.insert("ui_accent", "#5DB8F5");
        c.insert("ui_label", "#A9DFFF"); c.insert("ui_ok", "#4caf50");
        c.insert("ui_error", "#ef5350"); c.insert("ui_warn", "#ffa726");
        c.insert("prompt", "#EAF7FF"); c.insert("input_rule", "#2A6FB9");
        c.insert("response_border", "#5DB8F5"); c.insert("session_label", "#A9DFFF");
        c.insert("session_border", "#496884");
        let mut sp = HashMap::new();
        sp.insert("waiting_faces", vi(vec!["(≈)", "(Ψ)", "(∿)", "(◌)", "(◠)"]));
        sp.insert("thinking_faces", vi(vec!["(Ψ)", "(∿)", "(≈)", "(⌁)", "(◌)"]));
        sp.insert("thinking_verbs", vi(vec!["charting currents", "sounding the depth", "reading foam lines", "steering the trident", "tracking undertow", "plotting sea lanes", "calling the swell", "measuring pressure"]));
        sp.insert("wings", vv(vec![vec!["⟪≈", "≈⟫"], vec!["⟪Ψ", "Ψ⟫"], vec!["⟪∿", "∿⟫"], vec!["⟪◌", "◌⟫"]]));
        let logo = "[bold #B8E8FF]██████╗  ██████╗ ███████╗███████╗██╗██████╗  ██████╗ ███╗   ██╗       █████╗  ██████╗ ███████╗███╗   ██╗████████╗[/]\n[bold #97D6FF]██╔══██╗██╔═══██╗██╔════╝██╔════╝██║██╔══██╗██╔═══██╗████╗  ██║      ██╔══██╗██╔════╝ ██╔════╝████╗  ██║╚══██╔══╝[/]\n[#75C1F6]██████╔╝██║   ██║███████╗█████╗  ██║██║  ██║██║   ██║██╔██╗ ██║█████╗███████║██║  ███╗█████╗  ██╔██╗ ██║   ██║[/]\n[#4FA2E0]██╔═══╝ ██║   ██║╚════██║██╔══╝  ██║██║  ██║██║   ██║██║╚██╗██║╚════╝██╔══██║██║   ██║██╔══╝  ██║╚██╗██║   ██║[/]\n[#2E7CC7]██║     ╚██████╔╝███████║███████╗██║██████╔╝╚██████╔╝██║ ╚████║      ██║  ██║╚██████╔╝███████╗██║ ╚████║   ██║[/]\n[#1B4F95]╚═╝      ╚═════╝ ╚══════╝╚══════╝╚═╝╚═════╝  ╚═════╝ ╚═╝  ╚═══╝      ╚═╝  ╚═╝ ╚═════╝ ╚══════╝╚═╝  ╚═══╝   ╚═╝[/]";
        let hero = "[#2A6FB9]≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈[/]\n[#5DB8F5]≋≋≋≋≋≋≋≋≋≋≋≋≋≋≋[/]\n[#5DB8F5]≋≋≋≋≋≋Ψ≋≋≋≋≋≋≋[/]\n[#A9DFFF]≋≋≋≋≋≋≋≋≋≋≋≋≋≋≋[/]\n[#A9DFFF]≋≋≋≋≋≋≋≋≋≋≋≋≋≋≋[/]\n[#5DB8F5]≋≋≋≋≋≋≋≋≋≋≋≋≋≋≋[/]\n[#2A6FB9]≋≋≋≋≋≋≋≋≋≋≋≋≋≋≋[/]\n[#2A6FB9]≋≋≋≋≋≋≋≋≋≋≋≋≋≋≋[/]\n[#153C73]≋≋≋≋≋≋≋≋≋≋≋≋≋≋≋[/]\n[#153C73]≋≋≋≋≋≋≋≋≋≋≋≋≋≋≋[/]\n[#5DB8F5]≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈[/]\n[#A9DFFF]≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈[/]\n[dim #153C73]deep waters hold[/]";
        skins.insert("poseidon", skin_data!("poseidon", "Ocean-god theme — deep blue and seafoam", c, sp, vd(), "│", logo, hero));
    }

    // --- sisyphus ---
    {
        let mut c = HashMap::new();
        c.insert("banner_border", "#B7B7B7"); c.insert("banner_title", "#F5F5F5");
        c.insert("banner_accent", "#E7E7E7"); c.insert("banner_dim", "#4A4A4A");
        c.insert("banner_text", "#D3D3D3"); c.insert("ui_accent", "#E7E7E7");
        c.insert("ui_label", "#D3D3D3"); c.insert("ui_ok", "#919191");
        c.insert("ui_error", "#E7E7E7"); c.insert("ui_warn", "#B7B7B7");
        c.insert("prompt", "#F5F5F5"); c.insert("input_rule", "#656565");
        c.insert("response_border", "#B7B7B7"); c.insert("session_label", "#919191");
        c.insert("session_border", "#656565");
        let mut sp = HashMap::new();
        sp.insert("waiting_faces", vi(vec!["(◉)", "(◌)", "(◬)", "(⬤)", "(::)"]));
        sp.insert("thinking_faces", vi(vec!["(◉)", "(◬)", "(◌)", "(○)", "(●)"]));
        sp.insert("thinking_verbs", vi(vec!["finding traction", "measuring the grade", "resetting the boulder", "counting the ascent", "testing leverage", "setting the shoulder", "pushing uphill", "enduring the loop"]));
        sp.insert("wings", vv(vec![vec!["⟪◉", "◉⟫"], vec!["⟪◬", "◬⟫"], vec!["⟪◌", "◌⟫"], vec!["⟪⬤", "⬤⟫"]]));
        let logo = "[bold #F5F5F5]███████╗██╗███████╗██╗   ██╗██████╗ ██╗  ██╗██╗   ██╗███████╗       █████╗  ██████╗ ███████╗███╗   ██╗████████╗[/]\n[bold #E7E7E7]██╔════╝██║██╔════╝╚██╗ ██╔╝██╔══██╗██║  ██║██║   ██║██╔════╝      ██╔══██╗██╔════╝ ██╔════╝████╗  ██║╚══██╔══╝[/]\n[#D7D7D7]███████╗██║███████╗ ╚████╔╝ ██████╔╝███████║██║   ██║███████╗█████╗███████║██║  ███╗█████╗  ██╔██╗ ██║   ██║[/]\n[#BFBFBF]╚════██║██║╚════██║  ╚██╔╝  ██╔═══╝ ██╔══██║██║   ██║╚════██║╚════╝██╔══██║██║   ██║██╔══╝  ██║╚██╗██║   ██║[/]\n[#8F8F8F]███████║██║███████║   ██║   ██║     ██║  ██║╚██████╔╝███████║      ██║  ██║╚██████╔╝███████╗██║ ╚████║   ██║[/]\n[#626262]╚══════╝╚═╝╚══════╝   ╚═╝   ╚═╝     ╚═╝  ╚═╝ ╚═════╝ ╚══════╝      ╚═╝  ╚═╝ ╚═════╝ ╚══════╝╚═╝  ╚═══╝   ╚═╝[/]";
        let hero = "[#B7B7B7]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#D3D3D3]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#E7E7E7]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#F5F5F5]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#E7E7E7]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#D3D3D3]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#B7B7B7]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#919191]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#656565]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#656565]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#4A4A4A]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#4A4A4A]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#656565]━━━━━━━━━━━━━━━━━━━━━━[/]\n[dim #4A4A4A]the boulder[/]";
        skins.insert("sisyphus", skin_data!("sisyphus", "Sisyphean theme — austere grayscale with persistence", c, sp, vd(), "│", logo, hero));
    }

    // --- charizard ---
    {
        let mut c = HashMap::new();
        c.insert("banner_border", "#C75B1D"); c.insert("banner_title", "#FFD39A");
        c.insert("banner_accent", "#F29C38"); c.insert("banner_dim", "#7A3511");
        c.insert("banner_text", "#FFF0D4"); c.insert("ui_accent", "#F29C38");
        c.insert("ui_label", "#FFD39A"); c.insert("ui_ok", "#4caf50");
        c.insert("ui_error", "#ef5350"); c.insert("ui_warn", "#ffa726");
        c.insert("prompt", "#FFF0D4"); c.insert("input_rule", "#C75B1D");
        c.insert("response_border", "#F29C38"); c.insert("session_label", "#FFD39A");
        c.insert("session_border", "#6C4724");
        let mut sp = HashMap::new();
        sp.insert("waiting_faces", vi(vec!["(✦)", "(▲)", "(◇)", "(<>)", "(🔥)"]));
        sp.insert("thinking_faces", vi(vec!["(✦)", "(▲)", "(◇)", "(⌁)", "(🔥)"]));
        sp.insert("thinking_verbs", vi(vec!["banking into the draft", "measuring burn", "reading the updraft", "tracking ember fall", "setting wing angle", "holding the flame core", "plotting a hot landing", "coiling for lift"]));
        sp.insert("wings", vv(vec![vec!["⟪✦", "✦⟫"], vec!["⟪▲", "▲⟫"], vec!["⟪◌", "◌⟫"], vec!["⟪◇", "◇⟫"]]));
        let logo = "[bold #FFF0D4] ██████╗██╗  ██╗ █████╗ ██████╗ ██╗███████╗ █████╗ ██████╗ ██████╗        █████╗  ██████╗ ███████╗███╗   ██╗████████╗[/]\n[bold #FFD39A]██╔════╝██║  ██║██╔══██╗██╔══██╗██║╚══███╔╝██╔══██╗██╔══██╗██╔══██╗      ██╔══██╗██╔════╝ ██╔════╝████╗  ██║╚══██╔══╝[/]\n[#F29C38]██║     ███████║███████║██████╔╝██║  ███╔╝ ███████║██████╔╝██║  ██║█████╗███████║██║  ███╗█████╗  ██╔██╗ ██║   ██║[/]\n[#E2832B]██║     ██╔══██║██╔══██║██╔══██╗██║ ███╔╝  ██╔══██║██╔══██╗██║  ██║╚════╝██╔══██║██║   ██║██╔══╝  ██║╚██╗██║   ██║[/]\n[#C75B1D]╚██████╗██║  ██║██║  ██║██║  ██║██║███████╗██║  ██║██║  ██║██████╔╝      ██║  ██║╚██████╔╝███████╗██║ ╚████║   ██║[/]\n[#7A3511] ╚═════╝╚═╝  ╚═╝╚═╝  ╚═╝╚═╝  ╚═╝╚═╝╚══════╝╚═╝  ╚═╝╚═╝  ╚═╝╚═════╝       ╚═╝  ╚═╝ ╚═════╝ ╚══════╝╚═╝  ╚═══╝   ╚═╝[/]";
        let hero = "[#FFD39A]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#F29C38]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#F29C38]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#E2832B]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#E2832B]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#C75B1D]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#C75B1D]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#7A3511]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#7A3511]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#C75B1D]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#F29C38]━━━━━━━━━━━━━━━━━━━━━━[/]\n[#F29C38]━━━━━━━━━━━━━━━━━━━━━━[/]\n[dim #7A3511]tail flame lit[/]";
        skins.insert("charizard", skin_data!("charizard", "Volcanic theme — burnt orange and ember", c, sp, vd(), "│", logo, hero));
    }

    skins
}

static BUILTIN_SKINS: Lazy<HashMap<&'static str, SkinData>> = Lazy::new(make_builtin_skins);

static ACTIVE_SKIN_NAME: Lazy<RwLock<String>> = Lazy::new(|| RwLock::new(String::from("default")));

fn get_hermes_home() -> PathBuf {
    dirs::home_dir().map(|h| h.join(".hermes")).unwrap_or_else(|| PathBuf::from(".hermes"))
}
fn skins_dir() -> PathBuf { get_hermes_home().join("skins") }

fn load_skin_data_from_yaml(path: &PathBuf) -> Option<SkinData> {
    let content = fs::read_to_string(path).ok()?;
    serde_yaml::from_str(&content).ok()
}

fn build_skin_config(data: &SkinData) -> SkinConfig {
    let default_data = BUILTIN_SKINS.get("default").unwrap();

    let default_colors: HashMap<String, String> = default_data.get("colors")
        .and_then(|v: &YamlValue| serde_yaml::from_value(v.clone()).ok()).unwrap_or_default();
    let mut colors: HashMap<String, String> = default_colors;
    if let Some(new_colors) = data.get("colors") {
        if let Ok(c) = serde_yaml::from_value::<HashMap<String, String>>(new_colors.clone()) { colors.extend(c); }
    }

    let default_spinner: HashMap<String, YamlValue> = default_data.get("spinner")
        .and_then(|v: &YamlValue| serde_yaml::from_value(v.clone()).ok()).unwrap_or_default();
    let mut spinner: HashMap<String, YamlValue> = default_spinner;
    if let Some(new_spinner) = data.get("spinner") {
        if let Ok(s) = serde_yaml::from_value::<HashMap<String, YamlValue>>(new_spinner.clone()) { spinner.extend(s); }
    }

    let default_branding: HashMap<String, String> = default_data.get("branding")
        .and_then(|v: &YamlValue| serde_yaml::from_value(v.clone()).ok()).unwrap_or_default();
    let mut branding: HashMap<String, String> = default_branding;
    if let Some(new_branding) = data.get("branding") {
        if let Ok(b) = serde_yaml::from_value::<HashMap<String, String>>(new_branding.clone()) { branding.extend(b); }
    }

    let default_tool_prefix = default_data.get("tool_prefix").and_then(|v: &YamlValue| v.as_str()).unwrap_or("┊");
    let tool_prefix = data.get("tool_prefix").and_then(|v: &YamlValue| v.as_str()).unwrap_or(default_tool_prefix);

    let tool_emojis: HashMap<String, String> = data.get("tool_emojis")
        .and_then(|v: &YamlValue| serde_yaml::from_value(v.clone()).ok()).unwrap_or_default();

    let name = data.get("name").and_then(|v: &YamlValue| v.as_str()).unwrap_or("unknown");
    let description = data.get("description").and_then(|v: &YamlValue| v.as_str()).unwrap_or("");
    let banner_logo = data.get("banner_logo").and_then(|v: &YamlValue| v.as_str()).unwrap_or("");
    let banner_hero = data.get("banner_hero").and_then(|v: &YamlValue| v.as_str()).unwrap_or("");

    SkinConfig {
        name: name.to_string(), description: description.to_string(), colors, spinner, branding,
        tool_prefix: tool_prefix.to_string(), tool_emojis, banner_logo: banner_logo.to_string(), banner_hero: banner_hero.to_string(),
    }
}

fn do_load_skin(name: &str) -> SkinConfig {
    let user_file = skins_dir().join(format!("{}.yaml", name));
    if user_file.is_file() {
        if let Some(data) = load_skin_data_from_yaml(&user_file) { return build_skin_config(&data); }
    }
    if let Some(data) = BUILTIN_SKINS.get(name) { return build_skin_config(data); }
    build_skin_config(BUILTIN_SKINS.get("default").unwrap())
}

#[pymodule]
fn _skin_engine_rust(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(list_skins, m)?)?;
    m.add_function(wrap_pyfunction!(load_skin, m)?)?;
    m.add_function(wrap_pyfunction!(get_active_skin, m)?)?;
    m.add_function(wrap_pyfunction!(set_active_skin, m)?)?;
    m.add_function(wrap_pyfunction!(get_active_skin_name, m)?)?;
    m.add_function(wrap_pyfunction!(init_skin_from_config, m)?)?;
    m.add_function(wrap_pyfunction!(get_active_prompt_symbol, m)?)?;
    m.add_function(wrap_pyfunction!(get_active_help_header, m)?)?;
    m.add_function(wrap_pyfunction!(get_active_goodbye, m)?)?;
    m.add_function(wrap_pyfunction!(get_prompt_toolkit_style_overrides, m)?)?;
    Ok(())
}

#[pyfunction]
fn list_skins() -> Vec<HashMap<String, String>> {
    let mut result = Vec::new();
    for (name, data) in BUILTIN_SKINS.iter() {
        let description = data.get("description").and_then(|v: &YamlValue| v.as_str()).unwrap_or("");
        let mut map = HashMap::new();
        map.insert("name".to_string(), name.to_string());
        map.insert("description".to_string(), description.to_string());
        map.insert("source".to_string(), "builtin".to_string());
        result.push(map);
    }
    let skins_path = skins_dir();
    if skins_path.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&skins_path) {
            let mut user_skins: Vec<_> = entries.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map_or(false, |ext| ext == "yaml")).collect();
            user_skins.sort_by_key(|e| e.path());
            for entry in user_skins {
                if let Some(data) = load_skin_data_from_yaml(&entry.path()) {
                    let skin_name = data.get("name").and_then(|v: &YamlValue| v.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| entry.path().file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string());
                    if result.iter().any(|r| r.get("name") == Some(&skin_name)) { continue; }
                    let description = data.get("description").and_then(|v: &YamlValue| v.as_str()).unwrap_or("").to_string();
                    let mut map = HashMap::new();
                    map.insert("name".to_string(), skin_name);
                    map.insert("description".to_string(), description);
                    map.insert("source".to_string(), "user".to_string());
                    result.push(map);
                }
            }
        }
    }
    result
}

#[pyfunction] fn load_skin(name: String) -> SkinConfig { do_load_skin(&name) }
#[pyfunction] fn get_active_skin() -> SkinConfig { let name = ACTIVE_SKIN_NAME.read().unwrap().clone(); do_load_skin(&name) }
#[pyfunction] fn set_active_skin(name: String) -> SkinConfig { let mut n = ACTIVE_SKIN_NAME.write().unwrap(); *n = name.clone(); do_load_skin(&name) }
#[pyfunction] fn get_active_skin_name() -> String { ACTIVE_SKIN_NAME.read().unwrap().clone() }

#[pyfunction]
fn init_skin_from_config(_py: Python<'_>, config: &Bound<'_, PyDict>) -> PyResult<()> {
    let skin_name: String = if let Some(display) = config.get_item("display")? {
        if let Ok(d) = display.cast::<PyDict>() {
            if let Some(skin) = d.get_item("skin")? {
                if let Ok(s) = skin.extract::<String>() {
                    s
                } else { "default".to_string() }
            } else { "default".to_string() }
        } else { "default".to_string() }
    } else { "default".to_string() };
    let name = skin_name.trim().to_string();
    if !name.is_empty() { set_active_skin(name); } else { set_active_skin("default".to_string()); }
    Ok(())
}

#[pyfunction] fn get_active_prompt_symbol() -> String { let name = ACTIVE_SKIN_NAME.read().unwrap().clone(); do_load_skin(&name).get_branding_key("prompt_symbol", "❯ ") }
#[pyfunction] fn get_active_help_header() -> String { let name = ACTIVE_SKIN_NAME.read().unwrap().clone(); do_load_skin(&name).get_branding_key("help_header", "(^_^)? Available Commands") }
#[pyfunction] fn get_active_goodbye() -> String { let name = ACTIVE_SKIN_NAME.read().unwrap().clone(); do_load_skin(&name).get_branding_key("goodbye", "Goodbye! ⚕") }

#[pyfunction]
fn get_prompt_toolkit_style_overrides() -> HashMap<String, String> {
    let name = ACTIVE_SKIN_NAME.read().unwrap().clone();
    let config = do_load_skin(&name);
    let prompt = config.get_color("prompt", "#FFF8DC");
    let input_rule = config.get_color("input_rule", "#CD7F32");
    let title = config.get_color("banner_title", "#FFD700");
    let text = config.get_color("banner_text", &prompt);
    let dim = config.get_color("banner_dim", "#555555");
    let label = config.get_color("ui_label", &title);
    let warn = config.get_color("ui_warn", "#FF8C00");
    let error = config.get_color("ui_error", "#FF6B6B");
    let mut overrides = HashMap::new();
    overrides.insert("input-area".to_string(), prompt.clone());
    overrides.insert("placeholder".to_string(), format!("{} italic", dim));
    overrides.insert("prompt".to_string(), prompt.clone());
    overrides.insert("prompt-working".to_string(), format!("{} italic", dim));
    overrides.insert("hint".to_string(), format!("{} italic", dim));
    overrides.insert("input-rule".to_string(), input_rule.clone());
    overrides.insert("image-badge".to_string(), format!("{} bold", label));
    overrides.insert("completion-menu".to_string(), format!("bg:#1a1a2e {}", text));
    overrides.insert("completion-menu.completion".to_string(), format!("bg:#1a1a2e {}", text));
    overrides.insert("completion-menu.completion.current".to_string(), format!("bg:#333355 {}", title));
    overrides.insert("completion-menu.meta.completion".to_string(), format!("bg:#1a1a2e {}", dim));
    overrides.insert("completion-menu.meta.completion.current".to_string(), format!("bg:#333355 {}", label));
    overrides.insert("clarify-border".to_string(), input_rule.clone());
    overrides.insert("clarify-title".to_string(), format!("{} bold", title));
    overrides.insert("clarify-question".to_string(), format!("{} bold", text));
    overrides.insert("clarify-choice".to_string(), dim.clone());
    overrides.insert("clarify-selected".to_string(), format!("{} bold", title));
    overrides.insert("clarify-active-other".to_string(), format!("{} italic", title));
    overrides.insert("clarify-countdown".to_string(), input_rule.clone());
    overrides.insert("sudo-prompt".to_string(), format!("{} bold", error));
    overrides.insert("sudo-border".to_string(), input_rule.clone());
    overrides.insert("sudo-title".to_string(), format!("{} bold", error));
    overrides.insert("sudo-text".to_string(), text.clone());
    overrides.insert("approval-border".to_string(), input_rule.clone());
    overrides.insert("approval-title".to_string(), format!("{} bold", warn));
    overrides.insert("approval-desc".to_string(), format!("{} bold", text));
    overrides.insert("approval-cmd".to_string(), format!("{} italic", dim));
    overrides.insert("approval-choice".to_string(), dim.clone());
    overrides.insert("approval-selected".to_string(), format!("{} bold", title));
    overrides
}
