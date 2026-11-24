// ============================================================
//  SmartNet // dev_router.rs — протокол dev:// (Фаза 1).
//
//  Единый децентрализованный стандарт адресации для разработчиков:
//    dev://<username>                 → профиль автора (хаб репозиториев)
//    dev://<username>/<repo>          → страница проекта (dashboard)
//    dev://<username>/<repo>/<subsys> → подсистема проекта (code|issues|wiki)
//
//  Фаза 1: надёжный парсер URL + типизированные ошибки + Tauri-команды,
//  возвращающие MOCK-данные (симуляция ответов Sled DB / реестра имён).
//
//  Фаза 2 (заложено в структурах, см. пометки `PHASE 2`):
//    - resolve_username(name) → Ed25519-адрес автора из реестра SmartHoldem;
//    - dht.get_peers(repo_infohash) → поиск сидеров в Mainline DHT;
//    - iroh.download(blob) → загрузка git-объектов через Swarm-движок.
//
//  Author: TechnoL0g
//  Created: 2026-07-08
// ============================================================

use serde::Serialize;
use std::fmt;

/// Схема протокола. Вынесена в константу — никакого хардкода по месту.
const DEV_SCHEME: &str = "dev://";
/// Подсистема репозитория по умолчанию (открывается первой на Фазе 1).
const DEFAULT_SUBSYSTEM: &str = "code";

// ------------------------------------------------------------
//  Типизированные ошибки маршрутизатора
// ------------------------------------------------------------

/// Ошибки разбора dev://-URL. Сериализуются на фронтенд как объект
/// `{ kind, ...detail }`, чтобы UI мог показать точную причину.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum DevRouterError {
    /// Пустая строка URL.
    EmptyUrl,
    /// Схема не `dev://` (например, пришло `http://…`).
    WrongScheme { got: String },
    /// Не указано имя пользователя (например, `dev://`).
    MissingUsername,
    /// В сегменте недопустимые символы (разрешено: A–Z a–z 0–9 . _ -).
    InvalidCharacters { field: String, value: String },
    /// Слишком много сегментов пути (после subsystem ничего быть не должно).
    TooManySegments { path: String },
    /// Ресурс не найден в сети/кэше (используется командами данных).
    NotFound { what: String },
}

impl fmt::Display for DevRouterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DevRouterError::EmptyUrl => write!(f, "пустой dev://-URL"),
            DevRouterError::WrongScheme { got } => write!(f, "неверная схема: ожидалось dev://, получено '{}'", got),
            DevRouterError::MissingUsername => write!(f, "не указано имя пользователя"),
            DevRouterError::InvalidCharacters { field, value } => {
                write!(f, "недопустимые символы в поле '{}': '{}'", field, value)
            }
            DevRouterError::TooManySegments { path } => write!(f, "слишком длинный путь: '{}'", path),
            DevRouterError::NotFound { what } => write!(f, "не найдено: {}", what),
        }
    }
}

impl std::error::Error for DevRouterError {}

// ------------------------------------------------------------
//  Цель маршрутизации (результат парсинга)
// ------------------------------------------------------------

/// Куда ведёт разобранный dev://-URL. Тег `target` + поля — удобно матчить
/// на фронтенде (`switch (t.target) { ... }`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "target", rename_all = "snake_case")]
pub(crate) enum Target {
    /// Профиль разработчика: dev://<username>
    UserProfile { username: String },
    /// Конкретный репозиторий: dev://<username>/<repo>[/<subsystem>]
    Repository {
        username: String,
        repo: String,
        subsystem: String,
    },
}

// ------------------------------------------------------------
//  Модели данных (Phase-2-ready)
// ------------------------------------------------------------

/// Репозиторий, раздаваемый автором в P2P-сеть.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DevRepository {
    pub name: String,
    /// Адрес автора — для аватара в шапке репозитория.
    pub address: String,
    pub description: String,
    /// BLAKE3-хэш версии репозитория (address/name:updated); с приходом
    /// git-прокси станет хэшем HEAD-коммита.
    pub hash: String,
    pub updated_at: String,
    pub branches: Vec<String>,
    pub default_branch: String,
    pub stars: u32,
    pub commits: u32,
    pub language: String,
    /// Живые сидеры (Фаза 2: own=1; счёт из DHT-опроса — при P2P-клонировании).
    pub seeders: u32,
    /// SPDX-идентификатор лицензии ("MIT", …) или пусто.
    pub license: String,
    /// Релизы: ассеты живут в файлообменнике (f://), качаются ТОЛЬКО по клику.
    pub releases: Vec<crate::devhub::DevRelease>,
    /// Подписанные git-рефы (SHA последнего коммита каждой ветки).
    pub refs: Vec<crate::devhub::DevRef>,
    /// История коммитов (новые сверху, кап 50) — вкладка «Коммиты».
    pub log: Vec<crate::devhub::DevCommit>,
    /// «username/repo» источника форка (пусто, если это не fork).
    pub forked_from: String,
}

/// Карточка автора + его репозитории.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DevUser {
    pub username: String,
    /// SmartHoldem-адрес (Ed25519). PHASE 2: из resolve_username().
    pub address: String,
    pub bio: String,
    /// BLAKE3-CID аватара (для раздачи по iroh-blobs); пусто → identicon.
    pub avatar_cid: String,
    pub repositories: Vec<DevRepository>,
}

/// Результат глобального P2P-поиска проектов.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DevSearchResult {
    pub username: String,
    /// Адрес автора — для аватара в списках DevHub.
    pub address: String,
    pub repo: String,
    pub description: String,
    pub hash: String,
    pub stars: u32,
    /// PHASE 2: живые сидеры из Mainline DHT.
    pub seeders: u32,
}

// ------------------------------------------------------------
//  Парсер
// ------------------------------------------------------------

/// Разрешены только `A–Z a–z 0–9 . _ -`. Пустая строка недопустима.
pub(crate) fn sanitize_segment(field: &str, raw: &str) -> Result<String, DevRouterError> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(if field == "username" {
            DevRouterError::MissingUsername
        } else {
            DevRouterError::InvalidCharacters {
                field: field.to_string(),
                value: raw.to_string(),
            }
        });
    }
    let valid = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    if !valid {
        return Err(DevRouterError::InvalidCharacters {
            field: field.to_string(),
            value: s.to_string(),
        });
    }
    Ok(s.to_string())
}

/// Разобрать входящий dev://-URL в [`Target`]. Динамически, без хардкода.
///
/// ```text
/// dev://technolog              → UserProfile { username: "technolog" }
/// dev://technolog/smart-swarm  → Repository { technolog, smart-swarm, "code" }
/// dev://technolog/x/issues     → Repository { technolog, x, "issues" }
/// ```
pub(crate) fn parse(raw_url: &str) -> Result<Target, DevRouterError> {
    let url = raw_url.trim();
    if url.is_empty() {
        return Err(DevRouterError::EmptyUrl);
    }
    // Отсекаем query/fragment — на Фазе 1 они не используются.
    let url = url.split(['?', '#']).next().unwrap_or(url);

    let rest = match url.strip_prefix(DEV_SCHEME) {
        Some(r) => r,
        None => {
            let scheme = url.split("://").next().unwrap_or(url).to_string();
            return Err(DevRouterError::WrongScheme { got: scheme });
        }
    };

    // Нормализуем края: ведущие/замыкающие слэши не должны ломать разбор.
    let rest = rest.trim_matches('/');
    let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();

    match segments.as_slice() {
        [] => Err(DevRouterError::MissingUsername),
        [user] => Ok(Target::UserProfile {
            username: sanitize_segment("username", user)?,
        }),
        [user, repo] => Ok(Target::Repository {
            username: sanitize_segment("username", user)?,
            repo: sanitize_segment("repo", repo)?,
            subsystem: DEFAULT_SUBSYSTEM.to_string(),
        }),
        [user, repo, subsystem] => Ok(Target::Repository {
            username: sanitize_segment("username", user)?,
            repo: sanitize_segment("repo", repo)?,
            subsystem: sanitize_segment("subsystem", subsystem)?,
        }),
        _ => Err(DevRouterError::TooManySegments {
            path: rest.to_string(),
        }),
    }
}

// ------------------------------------------------------------
//  Фаза 2: реальные данные из подписанных DevHub-индексов
// ------------------------------------------------------------

/// DevRepoMeta (подписанный индекс) → DevRepository (модель UI).
fn to_repository(
    meta: &crate::devhub::DevRepoMeta,
    address: &str,
    own: bool,
    stars: u32,
) -> DevRepository {
    let hash = blake3::hash(format!("{}/{}:{}", address, meta.name, meta.updated).as_bytes())
        .to_hex()
        .to_string()[..40]
        .to_string();
    DevRepository {
        name: meta.name.clone(),
        address: address.to_string(),
        description: meta.description.clone(),
        hash,
        updated_at: meta.updated.to_string(),
        branches: meta.branches.clone(),
        default_branch: meta.default_branch.clone(),
        stars,
        commits: meta.commits,
        language: meta.language.clone(),
        seeders: if own {
            1
        } else {
            crate::devhub::seeders_for(address).max(1)
        },
        license: meta.license.clone(),
        releases: meta.releases.clone(),
        refs: meta.refs.clone(),
        log: meta.log.clone(),
        forked_from: meta.forked_from.clone(),
    }
}

/// Свой ли это адрес (мы — единственный гарантированный сидер своих репо).
fn is_own(address: &str) -> bool {
    crate::load_vault_internal().map(|v| v.address).as_deref() == Some(address)
}

// ------------------------------------------------------------
//  Tauri-команды
// ------------------------------------------------------------

/// Разобрать dev://-URL → [`Target`] (или типизированная ошибка). Используется
/// фронтендом при перехвате deep-link / клике по внутренней dev://-ссылке.
#[tauri::command]
pub(crate) async fn parse_dev_url(url: String) -> Result<Target, DevRouterError> {
    parse(&url)
}

/// Список репозиториев автора. Фаза 2: имя резолвится через on-chain реестр
/// SmartHoldem (naming.rs), репозитории берутся из подписанного DevHub-индекса
/// (свой — с диска, чужой — из верифицированного Sled-кэша).
#[tauri::command]
pub(crate) async fn get_user_repositories(
    app: tauri::AppHandle,
    username: String,
) -> Result<DevUser, DevRouterError> {
    let username = sanitize_segment("username", &username)?;
    let address = crate::devhub::resolve_dev_username(&username).ok_or(DevRouterError::NotFound {
        what: format!("имя @{} не зарегистрировано в on-chain реестре", username),
    })?;
    let own = is_own(&address);
    // Чужой автор: фоновый P2P-синк (DHT → биконы → swarm-загрузка индекса).
    // UI получает кэш мгновенно и обновится по событию `devhub-synced`.
    // Троттлится внутри sync_remote (1 раз/мин на автора) — сеть не флудится.
    if !own {
        let app2 = app.clone();
        let uname = username.clone();
        tauri::async_runtime::spawn(async move {
            let _ = crate::devhub::sync_remote(&app2, &uname).await;
        });
    }
    let repositories = crate::devhub::load_index(&app, &address)
        .map(|idx| {
            let stars = crate::devhub::star_counts(&app);
            idx.repos
                .iter()
                .map(|m| {
                    let key = format!("{}/{}", username, m.name);
                    to_repository(m, &address, own, *stars.get(&key).unwrap_or(&0))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(DevUser {
        address,
        bio: String::new(),
        avatar_cid: String::new(),
        repositories,
        username,
    })
}

/// Данные одного репозитория (страница проекта).
#[tauri::command]
pub(crate) async fn get_repository(
    app: tauri::AppHandle,
    username: String,
    repo: String,
) -> Result<DevRepository, DevRouterError> {
    let username = sanitize_segment("username", &username)?;
    let repo = sanitize_segment("repo", &repo)?;
    let address = crate::devhub::resolve_dev_username(&username).ok_or(DevRouterError::NotFound {
        what: format!("имя @{} не зарегистрировано", username),
    })?;
    let own = is_own(&address);

    // Локальный кэш → мгновенный ответ.
    let find = |app: &tauri::AppHandle| -> Option<DevRepository> {
        let stars = crate::devhub::star_counts(app);
        crate::devhub::load_index(app, &address).and_then(|idx| {
            idx.repos.iter().find(|m| m.name == repo).map(|m| {
                let key = format!("{}/{}", username, m.name);
                to_repository(m, &address, own, *stars.get(&key).unwrap_or(&0))
            })
        })
    };
    if let Some(r) = find(&app) {
        return Ok(r);
    }

    // Промах: у ЧУЖОГО автора пробуем сетевой синк (DHT → биконы → swarm),
    // как это уже делает страница профиля. Blocking-попытка с таймаутом, чтобы
    // прямое открытие dev://user/repo часто срабатывало сразу; при истечении
    // синк продолжается в фоне и UI обновится по событию `devhub-synced`.
    if !own {
        let synced = tokio::time::timeout(
            std::time::Duration::from_secs(6),
            crate::devhub::sync_remote_at(&app, &username, &address),
        )
        .await;
        match synced {
            Ok(_) => {
                if let Some(r) = find(&app) {
                    return Ok(r);
                }
            }
            Err(_) => {
                // не успели за 6 c — догоняем в фоне (событие обновит страницу)
                let app2 = app.clone();
                let uname = username.clone();
                let addr = address.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = crate::devhub::sync_remote_at(&app2, &uname, &addr).await;
                });
            }
        }
    }

    Err(DevRouterError::NotFound {
        what: format!("репозиторий {}/{}", username, repo),
    })
}

/// Глобальный поиск проектов по локально известным подписанным индексам
/// (свой + верифицированный кэш чужих авторов). С приходом P2P-синка индексов
/// выдача будет пополняться автоматически.
#[tauri::command]
pub(crate) async fn search_dev_network(app: tauri::AppHandle, query: String) -> Vec<DevSearchResult> {
    let q = query.trim().to_lowercase();
    let mut out: Vec<DevSearchResult> = Vec::new();
    let stars = crate::devhub::star_counts(&app);
    for idx in crate::devhub::all_known_indexes(&app) {
        let own = is_own(&idx.address);
        for m in &idx.repos {
            let matched = q.is_empty()
                || idx.username.to_lowercase().contains(&q)
                || m.name.to_lowercase().contains(&q)
                || m.description.to_lowercase().contains(&q);
            if matched {
                let key = format!("{}/{}", idx.username, m.name);
                let r = to_repository(m, &idx.address, own, *stars.get(&key).unwrap_or(&0));
                out.push(DevSearchResult {
                    username: idx.username.clone(),
                    address: idx.address.clone(),
                    repo: m.name.clone(),
                    description: m.description.clone(),
                    hash: r.hash,
                    stars: r.stars,
                    seeders: r.seeders,
                });
            }
        }
    }
    // Честный рейтинг: сперва подписанные стары, затем живые сидеры.
    out.sort_by(|a, b| b.stars.cmp(&a.stars).then(b.seeders.cmp(&a.seeders)).then(a.repo.cmp(&b.repo)));
    out
}

// ------------------------------------------------------------
//  Тесты парсера
// ------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_profile() {
        assert_eq!(
            parse("dev://technolog").unwrap(),
            Target::UserProfile { username: "technolog".into() }
        );
    }

    #[test]
    fn parses_repository_with_default_subsystem() {
        assert_eq!(
            parse("dev://technolog/smart-swarm").unwrap(),
            Target::Repository {
                username: "technolog".into(),
                repo: "smart-swarm".into(),
                subsystem: "code".into(),
            }
        );
    }

    #[test]
    fn parses_repository_with_subsystem() {
        assert_eq!(
            parse("dev://technolog/x/issues").unwrap(),
            Target::Repository {
                username: "technolog".into(),
                repo: "x".into(),
                subsystem: "issues".into(),
            }
        );
    }

    #[test]
    fn rejects_wrong_scheme() {
        assert!(matches!(parse("http://x"), Err(DevRouterError::WrongScheme { .. })));
    }

    #[test]
    fn rejects_invalid_chars() {
        assert!(matches!(
            parse("dev://bad name"),
            Err(DevRouterError::InvalidCharacters { .. })
        ));
    }

    #[test]
    fn rejects_empty_and_missing_user() {
        assert!(matches!(parse("   "), Err(DevRouterError::EmptyUrl)));
        assert!(matches!(parse("dev://"), Err(DevRouterError::MissingUsername)));
    }
}
