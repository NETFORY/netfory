//! Конфигурация AI-модели переводов.
//!
//! Настройка через `frontend/.env` — минимум переменных, максимум пресетов:
//!
//! ```env
//! # Выбор пресета (полный каталог — в функции `preset()` ниже).
//! # По умолчанию: qwen25-1.5b (стабильный, iroh + HTTPS fallback, ~1.9 ГБ).
//! TRANSLATOR_MODEL=qwen25-1.5b
//!
//! # (опционально) переопределить HTTPS-зеркало модели активного пресета.
//! # Полезно, если HuggingFace недоступен и у вас есть собственный mirror.
//! # Всё остальное (iroh CID, токенизатор, архитектура, EOS-токены и т.д.)
//! # остаётся из пресета — не нужно копировать десятки полей руками.
//! # TRANSLATOR_MODEL_URL=https://mirror.example/model.gguf
//! ```
//!
//! Пресеты — единственное место, где живут iroh CID/nodeId, размеры,
//! архитектура (Qwen2/Qwen3), chat-template и EOS-токены. Приоритет
//! источников при загрузке — HTTPS URL основной, iroh CID fallback.

use once_cell::sync::Lazy;

/// Архитектура модели → определяет, какой candle-модуль грузит веса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Architecture {
    /// candle::quantized_qwen2 (Qwen2, Qwen2.5)
    Qwen2,
    /// candle::quantized_qwen3 (Qwen3, включая Bonsai-*-unpacked при обычной квантизации)
    Qwen3,
}

/// Chat-шаблон для формирования промпта. Все текущие пресеты используют
/// ChatMLQwen — общий для Qwen2/2.5/3. Новые семейства → новый вариант enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatTemplate {
    /// ChatML c `<|im_start|>` / `<|im_end|>` — Qwen2, Qwen2.5, Qwen3.
    ChatMlQwen,
}

/// Движок инференса, которым грузится модель.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EngineKind {
    /// candle quantized (Qwen2/Qwen3, стандартные GGUF-типы Q4_K/Q8_0 и т.п.).
    Candle,
    /// FFI к llama.cpp (крейт `llama-cpp-2`) — нужен для 1-битного Q1_0 (Bonsai).
    /// Активен только в сборке с фичей `bonsai` (см. Cargo.toml).
    LlamaCpp,
}

/// Полная конфигурация модели, применяемая в `translator.rs`.
/// Пользователь конфигурирует ТОЛЬКО имя пресета через `TRANSLATOR_MODEL`;
/// всё остальное живёт в этом файле, менять напрямую в env не нужно.
#[derive(Debug, Clone)]
pub(crate) struct ModelConfig {
    pub name: &'static str,
    pub description: &'static str,
    pub model_file: &'static str,
    /// Прямой HTTPS URL к GGUF (основной путь). Может быть переопределён
    /// через `TRANSLATOR_MODEL_URL` — единственный env-override модели.
    pub model_url: String,
    /// iroh CID модели (fallback P2P, "торрент" в терминологии SmartNet).
    pub model_iroh_cid: &'static str,
    /// nodeId сидера iroh (обязателен, если задан CID).
    pub model_iroh_node: &'static str,
    pub model_size: u64,
    pub tokenizer_file: &'static str,
    pub tokenizer_url: &'static str,
    pub tokenizer_iroh_cid: &'static str,
    pub tokenizer_iroh_node: &'static str,
    pub tokenizer_size: u64,
    pub architecture: Architecture,
    pub chat_template: ChatTemplate,
    /// Каким движком грузить модель (candle vs llama.cpp FFI).
    pub engine: EngineKind,
    /// EOS-токены; любой из них останавливает генерацию.
    pub eos_tokens: &'static [u32],
    pub required_disk_bytes: u64,
    /// Модель поддерживается candle 0.9? Bonsai (Q1_0) — false.
    pub supported: bool,
    pub unsupported_reason: &'static str,
}

// ── Встроенные пресеты ────────────────────────────────────────────────────
//
// Общий узел-сидер SmartNet для pinned моделей, доступных «из коробки»
// через iroh-раздачу (тот же, что в v1.85–v1.93).
const DEFAULT_IROH_NODE: &str =
    "a06678f058919e28073eada3ad7534a93f488a85ba12ab159286f382e5e95d92";

// Стандартные EOS-токены Qwen (<|im_end|>=151645, <|endoftext|>=151643) —
// одинаковы для Qwen2, Qwen2.5 и Qwen3.
const QWEN_EOS: &[u32] = &[151645, 151643];

fn preset(name: &str) -> Option<ModelConfig> {
    match name.trim().to_lowercase().as_str() {
        // Дефолт: проверенный Qwen2.5-1.5B Q8_0. HTTPS с HuggingFace + iroh fallback.
        "qwen25-1.5b" | "" => Some(ModelConfig {
            name: "qwen25-1.5b",
            engine: EngineKind::Candle,
            description: "Qwen2.5-1.5B-Instruct GGUF Q8_0 — стабильный дефолт SmartNet (~1.9 ГБ)",
            model_file: "Qwen2.5-1.5B-Instruct-q8_0.gguf",
            model_url: "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q8_0.gguf".to_string(),
            model_iroh_cid: "dcb483e27cfee8a7d2236b71562985ffff3057763e4c13fdb21bd8adfc7847bb",
            model_iroh_node: DEFAULT_IROH_NODE,
            model_size: 1_894_532_128,
            tokenizer_file: "tokenizer.json",
            tokenizer_url: "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct/resolve/main/tokenizer.json",
            tokenizer_iroh_cid: "b91332ca0c7a5f8e173effc53337026f64f17d4f25ca09205d4c1d5ecae4d621",
            tokenizer_iroh_node: DEFAULT_IROH_NODE,
            tokenizer_size: 7_031_645,
            architecture: Architecture::Qwen2,
            chat_template: ChatTemplate::ChatMlQwen,
            eos_tokens: QWEN_EOS,
            required_disk_bytes: 1_500 * 1024 * 1024,
            supported: true,
            unsupported_reason: "",
        }),

        // Qwen3-1.7B Q8_0 (~1.9 GB) — новое поколение, HTTPS-only (пока не в iroh).
        "qwen3-1.7b" => Some(ModelConfig {
            name: "qwen3-1.7b",
            engine: EngineKind::Candle,
            description: "Qwen3-1.7B GGUF Q8_0 — новое поколение Qwen (~1.9 ГБ)",
            model_file: "Qwen3-1.7B-Q8_0.gguf",
            model_url: "https://huggingface.co/Qwen/Qwen3-1.7B-GGUF/resolve/main/Qwen3-1.7B-Q8_0.gguf".to_string(),
            model_iroh_cid: "",
            model_iroh_node: "",
            model_size: 1_834_385_824,
            tokenizer_file: "tokenizer.json",
            tokenizer_url: "https://huggingface.co/Qwen/Qwen3-1.7B/resolve/main/tokenizer.json",
            tokenizer_iroh_cid: "",
            tokenizer_iroh_node: "",
            tokenizer_size: 11_422_654,
            architecture: Architecture::Qwen3,
            chat_template: ChatTemplate::ChatMlQwen,
            eos_tokens: QWEN_EOS,
            required_disk_bytes: 1_500 * 1024 * 1024,
            supported: true,
            unsupported_reason: "",
        }),

        // Qwen3-4B Q4_K_M (~2.5 GB) — сбалансированный размер/качество.
        "qwen3-4b" => Some(ModelConfig {
            name: "qwen3-4b",
            engine: EngineKind::Candle,
            description: "Qwen3-4B GGUF Q4_K_M — сбалансированное качество/скорость (~2.5 ГБ)",
            model_file: "Qwen3-4B-Q4_K_M.gguf",
            model_url: "https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf".to_string(),
            model_iroh_cid: "",
            model_iroh_node: "",
            model_size: 2_497_281_408,
            tokenizer_file: "tokenizer.json",
            tokenizer_url: "https://huggingface.co/Qwen/Qwen3-4B/resolve/main/tokenizer.json",
            tokenizer_iroh_cid: "",
            tokenizer_iroh_node: "",
            tokenizer_size: 11_422_654,
            architecture: Architecture::Qwen3,
            chat_template: ChatTemplate::ChatMlQwen,
            eos_tokens: QWEN_EOS,
            required_disk_bytes: 3_000 * 1024 * 1024,
            supported: true,
            unsupported_reason: "",
        }),

        // Qwen3-8B Q4_K_M (~4.7 GB) — максимум качества, медленно на CPU.
        "qwen3-8b" => Some(ModelConfig {
            name: "qwen3-8b",
            engine: EngineKind::Candle,
            description: "Qwen3-8B GGUF Q4_K_M — топ качества, медленно на CPU (~4.7 ГБ)",
            model_file: "Qwen3-8B-Q4_K_M.gguf",
            model_url: "https://huggingface.co/Qwen/Qwen3-8B-GGUF/resolve/main/Qwen3-8B-Q4_K_M.gguf".to_string(),
            model_iroh_cid: "",
            model_iroh_node: "",
            model_size: 4_683_073_248,
            tokenizer_file: "tokenizer.json",
            tokenizer_url: "https://huggingface.co/Qwen/Qwen3-8B/resolve/main/tokenizer.json",
            tokenizer_iroh_cid: "",
            tokenizer_iroh_node: "",
            tokenizer_size: 11_422_654,
            architecture: Architecture::Qwen3,
            chat_template: ChatTemplate::ChatMlQwen,
            eos_tokens: QWEN_EOS,
            required_disk_bytes: 5_500 * 1024 * 1024,
            supported: true,
            unsupported_reason: "",
        }),

        // Bonsai-4B (Q1_0) — 1-bit, движок llama.cpp (FFI). candle не читает Q1_0.
        // supported = true ТОЛЬКО в сборке с фичей `bonsai`; иначе UI мягко блокирует.
        "bonsai-4b" => Some(ModelConfig {
            name: "bonsai-4b",
            engine: EngineKind::LlamaCpp,
            description: "Bonsai-4B 1-bit (Q1_0) — Qwen3-4B архитектура, ~0.57 ГБ; движок llama.cpp (сборка --features bonsai)",
            model_file: "Bonsai-4B-Q1_0.gguf",
            model_url: "https://huggingface.co/prism-ml/Bonsai-4B-gguf/resolve/main/Bonsai-4B-Q1_0.gguf".to_string(),
            model_iroh_cid: "b75cf8c870130b39c0a136e5579e6aa3a68c1d1e663faaa178cd259cf3b3e3fe",
            model_iroh_node: DEFAULT_IROH_NODE,
            model_size: 572_270_624,
            // llama.cpp берёт словарь прямо из GGUF — отдельный tokenizer.json не нужен.
            tokenizer_file: "",
            tokenizer_url: "",
            tokenizer_iroh_cid: "",
            tokenizer_iroh_node: "",
            tokenizer_size: 0,
            architecture: Architecture::Qwen3,
            chat_template: ChatTemplate::ChatMlQwen,
            eos_tokens: QWEN_EOS,
            required_disk_bytes: 1_000 * 1024 * 1024,
            supported: cfg!(feature = "bonsai"),
            unsupported_reason:
                "Bonsai использует 1-битный GGUF-формат Q1_0 — стандартный движок candle его не читает. \
                 Пересоберите клиент с флагом `--features bonsai` (FFI к llama.cpp) — тогда Bonsai \
                 загрузится и заработает. Без флага используйте `qwen3-4b` (та же архитектура Qwen3-4B).",
        }),

        // Bonsai-8B (Q1_0) — та же схема, что и 4B (движок llama.cpp).
        "bonsai-8b" => Some(ModelConfig {
            name: "bonsai-8b",
            engine: EngineKind::LlamaCpp,
            description: "Bonsai-8B 1-bit (Q1_0) — Qwen3-8B архитектура, ~1.15 ГБ; движок llama.cpp (сборка --features bonsai)",
            model_file: "Bonsai-8B-Q1_0.gguf",
            model_url: "https://huggingface.co/prism-ml/Bonsai-8B-gguf/resolve/main/Bonsai-8B-Q1_0.gguf".to_string(),
            model_iroh_cid: "",
            model_iroh_node: "",
            model_size: 1_160_000_000,
            tokenizer_file: "",
            tokenizer_url: "",
            tokenizer_iroh_cid: "",
            tokenizer_iroh_node: "",
            tokenizer_size: 0,
            architecture: Architecture::Qwen3,
            chat_template: ChatTemplate::ChatMlQwen,
            eos_tokens: QWEN_EOS,
            required_disk_bytes: 1_500 * 1024 * 1024,
            supported: cfg!(feature = "bonsai"),
            unsupported_reason:
                "Bonsai использует 1-битный GGUF-формат Q1_0 — стандартный движок candle его не читает. \
                 Пересоберите клиент с флагом `--features bonsai` (FFI к llama.cpp) — тогда Bonsai \
                 загрузится и заработает. Без флага используйте `qwen3-8b` (та же архитектура Qwen3-8B).",
        }),

        _ => None,
    }
}

/// Названия всех встроенных пресетов (для UI списка и валидации).
pub(crate) fn preset_names() -> &'static [&'static str] {
    &[
        "qwen25-1.5b",
        "qwen3-1.7b",
        "qwen3-4b",
        "qwen3-8b",
        "bonsai-4b",
        "bonsai-8b",
    ]
}

/// Лёгкий read-only взгляд на пресет (для UI-списка). None — если пресет неизвестен.
pub(crate) fn preset_view(name: &str) -> Option<(&'static str, &'static str, bool, &'static str)> {
    let p = preset(name)?;
    Some((p.name, p.description, p.supported, p.unsupported_reason))
}

// ── Загрузка из окружения ─────────────────────────────────────────────────
//
// Ровно ДВЕ переменные:
//   TRANSLATOR_MODEL      — имя пресета (дефолт: qwen25-1.5b)
//   TRANSLATOR_MODEL_URL  — переопределить HTTPS URL основной модели пресета
//                           (для операторов с приватным зеркалом HuggingFace).
// Всё остальное (CID, размеры, архитектура, EOS-токены и т.п.) — данные
// пресета, не пользовательская настройка.

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().and_then(|v| {
        let t = v.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    })
}

fn build_config() -> ModelConfig {
    let base_name = env_var("TRANSLATOR_MODEL").unwrap_or_else(|| "qwen25-1.5b".to_string());
    let mut cfg = preset(&base_name).unwrap_or_else(|| {
        preset("qwen25-1.5b").expect("дефолтный пресет qwen25-1.5b должен существовать")
    });
    if let Some(v) = env_var("TRANSLATOR_MODEL_URL") {
        cfg.model_url = v;
    }
    cfg
}

/// Ленивая единая точка доступа к конфигу (кэшируется на весь runtime).
static CONFIG: Lazy<ModelConfig> = Lazy::new(build_config);

pub(crate) fn config() -> &'static ModelConfig {
    &CONFIG
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bonsai_presets_route_to_llamacpp_without_tokenizer() {
        for name in ["bonsai-4b", "bonsai-8b"] {
            let p = preset(name).expect("bonsai preset exists");
            assert_eq!(p.engine, EngineKind::LlamaCpp, "{name} → llama.cpp");
            // llama.cpp берёт словарь из GGUF — отдельный tokenizer.json не качаем.
            assert!(p.tokenizer_url.is_empty(), "{name}: no tokenizer url");
            assert!(p.tokenizer_file.is_empty(), "{name}: no tokenizer file");
            assert!(p.model_file.ends_with("Q1_0.gguf"), "{name}: Q1_0 gguf");
            // supported отражает наличие фичи bonsai в сборке.
            assert_eq!(p.supported, cfg!(feature = "bonsai"), "{name} supported=feature");
        }
    }

    #[test]
    fn qwen_presets_stay_on_candle_and_supported() {
        for name in ["qwen25-1.5b", "qwen3-1.7b", "qwen3-4b", "qwen3-8b"] {
            let p = preset(name).expect("qwen preset exists");
            assert_eq!(p.engine, EngineKind::Candle, "{name} → candle");
            assert!(p.supported, "{name} always supported");
            assert!(!p.tokenizer_url.is_empty(), "{name} has tokenizer");
        }
    }

    #[test]
    fn bonsai8b_default_disk_and_size_sane() {
        let p = preset("bonsai-8b").unwrap();
        assert_eq!(p.model_file, "Bonsai-8B-Q1_0.gguf");
        assert!(p.model_url.contains("prism-ml/Bonsai-8B-gguf"));
        assert!(p.model_size > 1_000_000_000);
    }
}
