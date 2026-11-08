//! GNU MO 目录读取，替代 Python 的 `gettext` 而不引入 libintl。
//!
//! 原项目只用 `gettext.gettext`（`NetworkMgr/trayicon.py:56`），没有 `ngettext`，
//! 所以这里只做单数查找；复数条目取第一种形态保留，避免整份文件解析失败。
//!
//! MO 格式固定：magic + 修订号 + 字符串数 + 原串表偏移 + 译串表偏移 + 哈希表，
//! 两张表都是 `(长度, 偏移)` 的数组，字符串不带 NUL 终止符。因此不需要任何
//! 第三方 crate，也不会给 FreeBSD port 增加 C 依赖。

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// gettext 域，与 `trayicon.py` 的 `textdomain('NetworkMgr')` 一致。
pub const DOMAIN: &str = "NetworkMgr-rust";

/// FreeBSD 上 `.mo` 的安装根目录，与 `setup.py` 和数据文件布局一致。
pub const DEFAULT_LOCALE_DIR: &str = "/usr/local/share/locale";

/// `.mo` 解析失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoError {
    /// 文件短于 20 字节的头部。
    TooShort,
    /// 前四个字节既不是小端也不是大端的 MO magic。
    BadMagic(u32),
    /// 偏移表或字符串越界。
    Truncated,
}

impl fmt::Display for MoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort => write!(f, "MO file is shorter than its 20-byte header"),
            Self::BadMagic(magic) => write!(f, "not a MO file: magic {magic:#010x}"),
            Self::Truncated => write!(f, "MO file is truncated"),
        }
    }
}

impl std::error::Error for MoError {}

/// 一份翻译目录。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    entries: BTreeMap<String, String>,
}

impl Catalog {
    /// 解析 `.mo` 字节。
    pub fn parse(bytes: &[u8]) -> Result<Self, MoError> {
        if bytes.len() < 20 {
            return Err(MoError::TooShort);
        }
        // 文件里 magic 按自身字节序存储：小端文件读作 LE 得到 0x950412de，
        // 大端文件读作 LE 得到 0xde120495。
        let magic = u32::from_le_bytes(read_array(bytes, 0)?);
        let big_endian = match magic {
            0x9504_12de => false,
            0xde12_0495 => true,
            other => return Err(MoError::BadMagic(other)),
        };

        let count = read_u32(bytes, 8, big_endian)? as usize;
        let originals = read_u32(bytes, 12, big_endian)? as usize;
        let translations = read_u32(bytes, 16, big_endian)? as usize;

        let mut entries = BTreeMap::new();
        for index in 0..count {
            let original = read_pair(bytes, originals, index, big_endian)?;
            let translation = read_pair(bytes, translations, index, big_endian)?;
            let msgid = read_string(bytes, original)?;
            let msgstr = read_string(bytes, translation)?;

            // 复数条目是 "msgid\0msgid_plural"，取单数部分与第一种译文。
            let key = msgid.split('\0').next().unwrap_or("").to_string();
            let value = msgstr.split('\0').next().unwrap_or("").to_string();
            if !key.is_empty() || !value.is_empty() {
                entries.insert(key, value);
            }
        }
        Ok(Self { entries })
    }

    /// 查一条翻译；没有时原样返回 msgid（与 gettext 行为一致）。
    pub fn translate<'a>(&'a self, msgid: &'a str) -> &'a str {
        self.entries.get(msgid).map(String::as_str).unwrap_or(msgid)
    }

    /// MO 头部元数据（`""` 条目的译文），含 `Language:`、`Plural-Forms:` 等。
    pub fn header(&self) -> &str {
        self.entries.get("").map(String::as_str).unwrap_or("")
    }

    /// 遍历 (msgid, msgstr)，含头部元数据那条空 msgid。
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|(msgid, msgstr)| (msgid.as_str(), msgstr.as_str()))
    }

    /// 条目数，含头部元数据。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否没有任何条目。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// `{locale_dir}/{lang}/LC_MESSAGES/NetworkMgr.mo`。
pub fn catalog_path(locale_dir: &Path, lang: &str) -> PathBuf {
    locale_dir
        .join(lang)
        .join("LC_MESSAGES")
        .join(format!("{DOMAIN}.mo"))
}

/// 读取指定语言的目录；文件缺失或损坏时返回 None，让调用方回退原文。
pub fn load(locale_dir: &Path, lang: &str) -> Option<Catalog> {
    let path = catalog_path(locale_dir, lang);
    let bytes = fs::read(path).ok()?;
    Catalog::parse(&bytes).ok()
}

/// 把 `LANG` / `LC_MESSAGES` 的值收敛成 gettext 的语言名。
///
/// `zh_CN.UTF-8` → `zh_CN`；`C` / `POSIX` / 空值 → None（表示不翻译）。
pub fn detect_lang(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let base = raw.split(['.', '@']).next().unwrap_or("");
    if base.is_empty() || base == "C" || base == "POSIX" {
        return None;
    }
    Some(base.to_string())
}

/// 有目录就翻译，没有就返回原文。
pub fn translate_or<'a>(msgid: &'a str, catalog: Option<&'a Catalog>) -> &'a str {
    match catalog {
        Some(catalog) => catalog.translate(msgid),
        None => msgid,
    }
}

/// 覆盖 `.mo` 安装根目录，格式仍是 `<dir>/<lang>/LC_MESSAGES/NetworkMgr.mo`。
///
/// 用于未安装的运行、测试和截图；正常安装时不需要设置。
pub const LOCALE_DIR_ENV: &str = "NETWORKMGR_LOCALE_DIR";

/// 直接指定一份 `.mo`，优先于 [`LOCALE_DIR_ENV`]。
pub const MO_ENV: &str = "NETWORKMGR_MO";

/// 进程级翻译目录，首次 [`init`] 时加载。
static CATALOG: std::sync::OnceLock<Option<Catalog>> = std::sync::OnceLock::new();

/// 按 `LC_ALL` / `LC_MESSAGES` / `LANG` 的顺序选择语言并加载翻译。
///
/// 必须在任何 `tr!` 之前调用一次。重复调用是幂等的：`OnceLock` 保证
/// 多线程下只加载一次，后一次不会覆盖前一次的结果。找不到 `.mo` 时
/// 保持 `None`，所有消息回退到原文——翻译缺失不能阻止程序启动。
pub fn init() {
    CATALOG.get_or_init(load_from_env);
}

/// 翻译一条消息；未初始化或没有译文时返回原文。
///
/// 参数必须是 `'static` 字面量，因为返回的引用可能直接指向翻译表。
/// 动态内容用 gettext 的 `%s` 模板：`tr!("Configure %s").replace("%s", card)`。
pub fn tr(msgid: &'static str) -> &'static str {
    match CATALOG.get().and_then(Option::as_ref) {
        Some(catalog) => catalog.translate(msgid),
        None => msgid,
    }
}

fn load_from_env() -> Option<Catalog> {
    if let Some(path) = std::env::var_os(MO_ENV) {
        let bytes = fs::read(path).ok()?;
        return Catalog::parse(&bytes).ok();
    }
    let raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.trim().is_empty());
    let lang = detect_lang(raw.as_deref())?;
    let dir = std::env::var_os(LOCALE_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_LOCALE_DIR));
    load_with_fallback(&dir, &lang)
}

/// 先找完整语言名（`de_DE`），再退到基语言（`de`），与 gettext 的查找顺序一致。
fn load_with_fallback(dir: &Path, lang: &str) -> Option<Catalog> {
    if let Some(catalog) = load(dir, lang) {
        return Some(catalog);
    }
    let base = lang.split('_').next().unwrap_or(lang);
    if base == lang {
        None
    } else {
        load(dir, base)
    }
}

/// 翻译字面量消息，等价于 Python 的 `_()`。
///
/// ```ignore
/// use networkmgr_core::tr;
/// let label = tr!("Restart Networking");
/// ```
#[macro_export]
macro_rules! tr {
    ($msgid:expr) => {
        $crate::i18n::tr($msgid)
    };
}

fn read_array(bytes: &[u8], offset: usize) -> Result<[u8; 4], MoError> {
    let slice = bytes.get(offset..offset + 4).ok_or(MoError::Truncated)?;
    let mut array = [0u8; 4];
    array.copy_from_slice(slice);
    Ok(array)
}

fn read_u32(bytes: &[u8], offset: usize, big_endian: bool) -> Result<u32, MoError> {
    let array = read_array(bytes, offset)?;
    Ok(if big_endian {
        u32::from_be_bytes(array)
    } else {
        u32::from_le_bytes(array)
    })
}

/// 读第 `index` 个 `(长度, 偏移)`。
fn read_pair(
    bytes: &[u8],
    table: usize,
    index: usize,
    big_endian: bool,
) -> Result<(usize, usize), MoError> {
    let base = table + index * 8;
    let length = read_u32(bytes, base, big_endian)? as usize;
    let offset = read_u32(bytes, base + 4, big_endian)? as usize;
    Ok((length, offset))
}

fn read_string(bytes: &[u8], (length, offset): (usize, usize)) -> Result<String, MoError> {
    let slice = bytes
        .get(offset..offset + length)
        .ok_or(MoError::Truncated)?;
    Ok(String::from_utf8_lossy(slice).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一份最小 MO 文件，小端序。
    fn build_mo(entries: &[(&str, &str)]) -> Vec<u8> {
        let count = entries.len() as u32;
        let header_len = 28u32;
        let originals_table = header_len;
        let translations_table = originals_table + count * 8;
        let strings_start = translations_table + count * 8;

        let mut originals_blob = Vec::new();
        let mut translations_blob = Vec::new();
        let mut originals_table_bytes = Vec::new();
        let mut translations_table_bytes = Vec::new();

        // 布局是「全部 msgid 连着放，再放全部 msgstr」，所以译串的起点要用
        // 所有 msgid 的总长，而不是循环到当前位置已累积的长度。
        let originals_total: usize = entries.iter().map(|(msgid, _)| msgid.len()).sum();

        for (msgid, msgstr) in entries {
            originals_table_bytes.extend_from_slice(&(msgid.len() as u32).to_le_bytes());
            originals_table_bytes
                .extend_from_slice(&(strings_start + originals_blob.len() as u32).to_le_bytes());
            originals_blob.extend_from_slice(msgid.as_bytes());

            translations_table_bytes.extend_from_slice(&(msgstr.len() as u32).to_le_bytes());
            translations_table_bytes.extend_from_slice(
                &(strings_start + originals_total as u32 + translations_blob.len() as u32)
                    .to_le_bytes(),
            );
            translations_blob.extend_from_slice(msgstr.as_bytes());
        }

        let mut out = Vec::new();
        out.extend_from_slice(&0x9504_12deu32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&originals_table.to_le_bytes());
        out.extend_from_slice(&translations_table.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&originals_table_bytes);
        out.extend_from_slice(&translations_table_bytes);
        out.extend_from_slice(&originals_blob);
        out.extend_from_slice(&translations_blob);
        out
    }

    #[test]
    fn parses_and_translates_a_minimal_catalog() {
        let bytes = build_mo(&[
            ("", "Project-Id-Version: networkmgr 7.0\nLanguage: zh_CN\n"),
            ("Restart Networking", "重启网络"),
            ("Forget Network", "忘记网络"),
        ]);
        let catalog = Catalog::parse(&bytes).unwrap();
        assert_eq!(catalog.len(), 3);
        assert_eq!(catalog.translate("Restart Networking"), "重启网络");
        assert_eq!(catalog.translate("Forget Network"), "忘记网络");
        // 没有的条目原样返回
        assert_eq!(catalog.translate("Untranslated"), "Untranslated");
        assert!(catalog.header().contains("Language: zh_CN"));
    }

    #[test]
    fn plural_entries_keep_the_first_form() {
        let bytes = build_mo(&[("file\0files", "文件\0多个文件")]);
        let catalog = Catalog::parse(&bytes).unwrap();
        assert_eq!(catalog.translate("file"), "文件");
    }

    #[test]
    fn rejects_broken_files() {
        assert_eq!(Catalog::parse(&[]), Err(MoError::TooShort));
        assert_eq!(Catalog::parse(&[0u8; 20]), Err(MoError::BadMagic(0)));
        let mut truncated = build_mo(&[("a", "b")]);
        truncated.truncate(30);
        assert_eq!(Catalog::parse(&truncated), Err(MoError::Truncated));
    }

    #[test]
    fn lang_detection_strips_encoding_and_normalises_c() {
        assert_eq!(detect_lang(Some("zh_CN.UTF-8")).as_deref(), Some("zh_CN"));
        assert_eq!(detect_lang(Some("fr_FR@euro")).as_deref(), Some("fr_FR"));
        assert_eq!(detect_lang(Some("C")), None);
        assert_eq!(detect_lang(Some("POSIX")), None);
        assert_eq!(detect_lang(Some("  ")), None);
        assert_eq!(detect_lang(None), None);
    }

    #[test]
    fn catalog_path_matches_the_installed_layout() {
        let path = catalog_path(Path::new("/usr/local/share/locale"), "zh_CN");
        assert_eq!(
            path,
            PathBuf::from("/usr/local/share/locale/zh_CN/LC_MESSAGES/NetworkMgr-rust.mo")
        );
    }

    #[test]
    fn missing_catalog_falls_back_to_the_msgid() {
        let catalog = load(Path::new("/nonexistent/locale"), "zh_CN");
        assert!(catalog.is_none());
        assert_eq!(
            translate_or("Forget Network", catalog.as_ref()),
            "Forget Network"
        );
    }

    #[test]
    fn locale_falls_back_to_the_base_language() {
        let dir = std::env::temp_dir().join(format!("nm-i18n-{}", std::process::id()));
        let lang_dir = dir.join("de").join("LC_MESSAGES");
        std::fs::create_dir_all(&lang_dir).expect("temp locale dir");
        std::fs::write(
            lang_dir.join("NetworkMgr-rust.mo"),
            build_mo(&[("Apply", "Anwenden")]),
        )
        .expect("write mo");
        let catalog = load_with_fallback(&dir, "de_DE").expect("base language should load");
        assert_eq!(catalog.translate("Apply"), "Anwenden");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn translated_macro_falls_back_to_the_literal() {
        // 目录未初始化或没有这一条时，必须原样返回，不能 panic 或给出空串。
        assert_eq!(crate::tr!("Not In Any Catalog"), "Not In Any Catalog");
    }

    /// 全局入口的端到端校验：设置 `NETWORKMGR_MO` 后 `tr!` 必须返回译文。
    ///
    /// 平时跳过；验证时：
    ///   msgfmt po/zh_CN.po -o /tmp/zh_CN.mo
    ///   NETWORKMGR_MO=/tmp/zh_CN.mo cargo test global_translation
    #[test]
    fn global_translation_reads_the_environment_catalog() {
        if std::env::var_os(MO_ENV).is_none() {
            return;
        }
        init();
        assert_eq!(tr("Restart Networking"), "重启网络");
        assert_eq!(tr("Close Network Manager"), "退出网络管理器");
        assert_eq!(tr("Not In Any Catalog"), "Not In Any Catalog");
    }

    /// 用 `NETWORKMGR_MO=<path>` 指向真实编译出的 .mo 做端到端校验。
    ///
    /// 平时跳过（测试环境没有安装翻译），验证时：
    ///   msgfmt po/zh_CN.po -o /tmp/zh_CN.mo
    ///   NETWORKMGR_MO=/tmp/zh_CN.mo cargo test reads_a_real_catalog
    #[test]
    fn reads_a_real_catalog_when_available() {
        let Ok(path) = std::env::var("NETWORKMGR_MO") else {
            return;
        };
        let bytes = fs::read(&path).expect("NETWORKMGR_MO should point at a readable file");
        let catalog = Catalog::parse(&bytes).expect("compiled catalog should parse");
        assert!(catalog.len() > 1);
        assert!(catalog.header().contains("Language: zh_CN"));
        // 只断言「至少有一条真实译文」，不固化具体措辞：zh_CN 译文正在另一次
        // PR 里修订，且 fuzzy 条目会被 msgfmt 跳过，写死某一条会随分支失效。
        assert!(
            catalog
                .iter()
                .any(|(msgid, msgstr)| !msgid.is_empty() && !msgstr.is_empty() && msgid != msgstr),
            "expected at least one translated entry"
        );
        assert_eq!(
            catalog.translate("Not In This Catalog"),
            "Not In This Catalog"
        );
    }
}
