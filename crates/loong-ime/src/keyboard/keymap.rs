//! 每一层键盘上有哪些键，以及长按备选字符。
//!
//! regular（平板）尺寸类下，三层（字母 / 数字 / 符号）共用同一套网格，只换键帽内容：
//!
//! ```text
//! 第 1 行：10 个字符键                         + 退格
//! 第 2 行：（缩进半键）9 个字符键               + 换行
//! 第 3 行：左功能键 + 9 个字符键 + 右功能键      （字母层为 Shift；数字/符号层为换层键）
//! 第 4 行：换层键 | 中/英 | 空格 | 换层键 | 收起键盘
//! ```
//!
//! 这样换层时键的位置不变（与 iPad 一致），按下的高亮也能延续。

/// 键盘层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Layer {
    /// 字母（QWERTY）。
    #[default]
    Letters,
    /// 数字与常用标点（“123”）。
    Numbers,
    /// 更多符号（“#+=”）。
    Symbols,
}

/// 键的种类（决定行为；位置由布局决定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyKind {
    /// 字母键（小写存储，随 Shift 变大写）。中文模式下送入拼音组字。
    Letter(char),
    /// 字符键：直接输入这段文字（标点、数字、符号）。
    Char(&'static str),
    /// Shift（单击：下一个字母大写；双击：大写锁定）。
    Shift,
    /// 退格（按下即删除，按住连删）。
    Backspace,
    /// 换行 / 确认。
    Return,
    /// 空格（中文组字时选择高亮候选）。
    Space,
    /// 切换到某一层。
    Layer(Layer),
    /// 中/英切换。
    Language,
    /// 收起键盘。
    Hide,
}

impl KeyKind {
    /// 是否为功能键（iPad 上功能键用较深的底色）。
    pub fn is_function(&self) -> bool {
        !matches!(self, KeyKind::Letter(_) | KeyKind::Char(_) | KeyKind::Space)
    }

    /// 是否会输出字符（参与快速连打时的“提前上屏”）。
    pub(crate) fn types_text(&self) -> bool {
        matches!(self, KeyKind::Letter(_) | KeyKind::Char(_) | KeyKind::Space)
    }
}

/// 一层的全部键，按行给出（行内从左到右）。
pub(crate) struct LayerKeys {
    pub rows: [Vec<KeyKind>; 4],
}

fn chars(list: &[&'static str]) -> Vec<KeyKind> {
    list.iter().map(|s| KeyKind::Char(s)).collect()
}

fn letters(list: &str) -> Vec<KeyKind> {
    list.chars().map(KeyKind::Letter).collect()
}

/// 某一层（中文 / 英文模式）的键。
pub(crate) fn layer_keys(layer: Layer, chinese: bool) -> LayerKeys {
    let (row1, row2, row3_mid, side): (Vec<KeyKind>, Vec<KeyKind>, Vec<KeyKind>, KeyKind) =
        match (layer, chinese) {
            (Layer::Letters, _) => {
                let mut row3 = letters("zxcvbnm");
                row3.extend(if chinese {
                    chars(&["，", "。"])
                } else {
                    chars(&[",", "."])
                });
                (
                    letters("qwertyuiop"),
                    letters("asdfghjkl"),
                    row3,
                    KeyKind::Shift,
                )
            }
            (Layer::Numbers, false) => (
                chars(&["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"]),
                chars(&["-", "/", ":", ";", "(", ")", "$", "&", "@"]),
                chars(&[".", ",", "?", "!", "'", "\"", "%", "*", "#"]),
                KeyKind::Layer(Layer::Symbols),
            ),
            (Layer::Numbers, true) => (
                chars(&["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"]),
                chars(&["-", "/", "：", "；", "（", "）", "¥", "&", "@"]),
                // "'" 在拼音组字中是音节分隔符（xi'an → 西安）。
                chars(&[".", "，", "。", "、", "？", "！", "'", "“", "”"]),
                KeyKind::Layer(Layer::Symbols),
            ),
            (Layer::Symbols, false) => (
                chars(&["[", "]", "{", "}", "#", "%", "^", "*", "+", "="]),
                chars(&["_", "\\", "|", "~", "<", ">", "€", "£", "¥"]),
                chars(&["•", "…", "°", ".", ",", "?", "!", "'", "\""]),
                KeyKind::Layer(Layer::Numbers),
            ),
            (Layer::Symbols, true) => (
                chars(&["【", "】", "｛", "｝", "#", "%", "^", "*", "+", "="]),
                chars(&["_", "——", "\\", "|", "～", "《", "》", "$", "€"]),
                chars(&["·", "……", "°", "「", "」", "‘", "’", "<", ">"]),
                KeyKind::Layer(Layer::Numbers),
            ),
        };
    let back = if layer == Layer::Letters {
        KeyKind::Layer(Layer::Numbers)
    } else {
        KeyKind::Layer(Layer::Letters)
    };

    let mut r1 = row1;
    r1.push(KeyKind::Backspace);
    let mut r2 = row2;
    r2.push(KeyKind::Return);
    let mut r3 = vec![side];
    r3.extend(row3_mid);
    r3.push(side);
    let r4 = vec![back, KeyKind::Language, KeyKind::Space, back, KeyKind::Hide];
    debug_assert_eq!((r1.len(), r2.len(), r3.len()), (11, 10, 11));
    LayerKeys {
        rows: [r1, r2, r3, r4],
    }
}

/// 长按备选。第一个备选同时显示在键帽角上（例如 “，” 上方的 “！”）。
///
/// 字母备选只在英文模式下提供（拼音组字不需要）；大写时由调用方转成大写。
pub(crate) fn alternates(kind: KeyKind, chinese: bool) -> &'static [&'static str] {
    match kind {
        KeyKind::Letter(c) if !chinese => match c {
            'a' => &["à", "á", "â", "ä", "æ", "ã", "å", "ā"],
            'c' => &["ç", "ć", "č"],
            'e' => &["è", "é", "ê", "ë", "ē", "ę", "ė"],
            'i' => &["ì", "í", "î", "ï", "ī", "į"],
            'l' => &["ł"],
            'n' => &["ñ", "ń"],
            'o' => &["ò", "ó", "ô", "ö", "õ", "ø", "ō", "œ"],
            's' => &["ß", "ś", "š"],
            'u' => &["ù", "ú", "û", "ü", "ū"],
            'y' => &["ÿ"],
            'z' => &["ž", "ź", "ż"],
            _ => &[],
        },
        KeyKind::Char(s) => match s {
            // 中文标点：全角变体 + 对应的英文标点
            "，" => &["！", "、", "；", "：", ",", "!"],
            "。" => &["？", "……", ".", "?"],
            "？" => &["?"],
            "！" => &["!"],
            "：" => &[":"],
            "；" => &[";"],
            "（" => &["(", "【", "《"],
            "）" => &[")", "】", "》"],
            "“" => &["”", "\"", "「"],
            "”" => &["“", "\"", "」"],
            // 英文标点：常用变体 + 对应的中文标点
            "," => &["!", ";", ":", "，"],
            "." => &["?", "…", "。"],
            "?" => &["¿", "？"],
            "!" => &["¡", "！"],
            "-" => &["–", "—", "•"],
            "/" => &["\\"],
            "'" => &["’", "‘", "`"],
            "\"" => &["“", "”", "„", "«", "»"],
            "$" => &["¥", "€", "£", "₩", "¢"],
            "¥" => &["$", "€", "£"],
            "&" => &["§"],
            "%" => &["‰"],
            "0" => &["°"],
            _ => &[],
        },
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_layer_has_the_same_grid() {
        for layer in [Layer::Letters, Layer::Numbers, Layer::Symbols] {
            for chinese in [false, true] {
                let keys = layer_keys(layer, chinese);
                let lens: Vec<usize> = keys.rows.iter().map(Vec::len).collect();
                assert_eq!(lens, [11, 10, 11, 5], "{layer:?} 中文={chinese}");
            }
        }
    }

    #[test]
    fn chinese_letters_have_fullwidth_punctuation() {
        let keys = layer_keys(Layer::Letters, true);
        assert!(keys.rows[2].contains(&KeyKind::Char("，")));
        assert_eq!(alternates(KeyKind::Char("，"), true)[0], "！");
        assert!(alternates(KeyKind::Letter('a'), true).is_empty());
        assert!(!alternates(KeyKind::Letter('a'), false).is_empty());
    }
}
