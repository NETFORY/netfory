# ntfry - Zero-Knowledge encrypted cloud vault

`ntfry` - это библиотека и CLI для создания зашифрованного виртуального файлового
хранилища поверх облачных провайдеров. Реализация в проекте соответствует коду в
`ntfry/src/*`: индекс VFS хранится в памяти, чанки создаются по размеру
`CHUNK_SIZE` (по умолчанию 512 КБ, диапазон 64..4096 КБ), затем сжимаются
`ZSTD` (optional user dictionary), шифруются `ChaCha20-Poly1305` и загружаются в
облако как ` /ntfry/chunks/<sha256> `.

Провайдер не видит имена файлов, структуру каталогов и plaintext-данные. Он
видит только зашифрованные блоки и служебные JSON/файлы `/ntfry/meta`,
`/ntfry/index.enc`, `/ntfry/user.dict`.

```
[ локальный файл / UI / приложение ]
                │
                ▼
┌───────────────────────────────────────────────────────────────────┐
│ 1. VFS / in-memory index       (путь -> file entry + дедупликация)│
│ 2. ZSTD compression           (стандартный или user.dict)         │
│ 3. ChaCha20-Poly1305 AEAD     (BIP-39 -> BIP-32 master key)       │
│ 4. Storage adapter            (local / yandex / gdrive)           │
└───────────────────────────────────────────────────────────────────┘
```

## Как работает криптография

В коде используется строгая деривация ключей из BIP-39 мнемоники:

- `m/999'/0'/0'/0` -> мастер-ключ шифрования (`master_key`, 32 байта)
- `m/44'/60'/0'/0/0` -> ETH-совместимый адрес кошелька (`0x...`)

Ключи создаются через `crypto::derive()` / `crypto::derive_flexible()` и затем
используются для шифрования `VaultIndex` и файловых чанков.

Схема шифрования:

- nonce: 12 байт
- ciphertext + tag: `ChaCha20-Poly1305`
- зашифрованный индекс сохраняется как `index.enc`
- `meta` содержит ключевой отпечаток (`key_fingerprint`) для быстрой проверки,
  что облачное хранилище принадлежит тому же кошельку

## Структура удалённого хранилища

```text
/ntfry/
├── index.enc         # зашифрованный VaultIndex (bincode -> ChaCha20-Poly1305)
├── meta              # открытый отпечаток мастер-ключа (для проверки ключа)
├── user.dict         # пользовательский ZSTD-словарь, если задан
├── chunks/
│   ├── 4a8f9c...     # зашифрованный чанк, имя = sha256(encrypted chunk)
│   └── ...
└── ...
```

## Индекс и локальный кэш

`VaultIndex` хранит:

- `files`: путь -> `FileEntry`
- `dirs`: явно созданные каталоги
- `dedup`: `sha256(plaintext chunk)` -> идентификатор зашифрованного чанка
- `chunk_meta`: метаданные для точного пересчёта статистики
- `stats`: raw/stored/dedup-saved bytes

Локальный кэш создаётся через `sled`:

```text
./cache/<provider>/vault_index.sled
```

При открытии `Vault::open()` индекс сначала загружается из Sled, а затем валидируется
против облачного `index.enc` по SHA-256. Если облако изменилось, индекс
перекачивается и кэш обновляется. При обнаружении удаления `/ntfry` извне локальный
кэш сбрасывается, чтобы не "воскресить" удалённое хранилище старым индексом.

## Настройки и параметры

Код поддерживает следующие глобальные параметры CLI:

```bash
ntfry [--provider local|yandex|gdrive] [--mnemonic <phrase>] \
      [--local-root <path>] [--cache-dir <path>] [--dict <path>] <command>
```

- `--provider` - провайдер хранения (`local`, `yandex`, `gdrive`)
- `--mnemonic` - BIP-39 мнемоника (12/24 слова)
- `--local-root` - файловый "mock cloud" для локального провайдера
- `--cache-dir` - папка Sled-кэша
- `--dict` - локальный ZSTD-словарь, который переопределяет словарь хранилища

Есть также переменная окружения `CHUNK_SIZE` (в КБ), она читается один раз при
запуске процесса и ограничивается диапазоном `64..=4096`. По умолчанию используется
`512 * 1024` байт.

## CLI

Основные команды:

```bash
# 1) инициализация нового vault на локальном провайдере
ntfry --provider local init

# 2) инициализация с сохранённой мнемоникой
export NTFRY_MNEMONIC="abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
ntfry --provider yandex --mnemonic "$NTFRY_MNEMONIC" init

# 3) загрузка файла в VFS
ntfry --provider local push --src ./photo.jpg --dst /photos/photo.jpg

# 4) скачивание файла из VFS
ntfry --provider local pull --src /photos/photo.jpg --dst ./restored.jpg

# 5) листинг каталога
ntfry --provider local ls --path /photos

# 6) создание каталога в виртуальной FS
ntfry --provider local mkdir --path /archive

# 7) удаление файла или директории
ntfry --provider local rm --path /old.txt

# 8) обучение ZSTD-словаря на наборе файлов
ntfry --provider local --mnemonic "$NTFRY_MNEMONIC" \
  dict --train-from ./training-files --max-size 65536
```

### Переменные окружения

| Переменная | Назначение |
|---|---|
| `NTFRY_PROVIDER` | `local` \| `yandex` \| `gdrive` |
| `NTFRY_MNEMONIC` | BIP-39 мнемоника (12/24 слова) |
| `NTFRY_YANDEX_TOKEN` | OAuth token для Yandex Disk |
| `NTFRY_GDRIVE_TOKEN` | OAuth token для Google Drive |
| `NTFRY_LOCAL_ROOT` | корень локального "mock cloud", по умолчанию `./mock_cloud` |
| `NTFRY_CACHE_DIR` | каталог Sled-кэша, по умолчанию `./cache` |
| `NTFRY_DICT` | путь к локальному ZSTD-словарю |
| `CHUNK_SIZE` | размер чанка в КБ, `64..=4096`, по умолчанию 512 |

## Yandex OAuth

Для `--provider yandex` нужен OAuth-токен Yandex Disk. Его можно получить через
страницу авторизации:

```text
https://oauth.yandex.ru/authorize?response_type=token&client_id=<CLIENT_ID>
```

После подтверждения токен будет в URL как `#access_token=...`. Затем экспортируйте
его в переменную окружения:

```bash
export NTFRY_YANDEX_TOKEN="y0_AgA..."
```

## Что реализовано в коде сейчас

На сегодняшний день проект поддерживает:

- локальный провайдер (`LocalStorageProvider`)
- Yandex Disk (`YandexDiskProvider`)
- Google Drive (`GoogleDriveProvider`)
- обучение и использование пользовательского `user.dict`
- дедупликацию по SHA-256 plaintext-чанков
- точный пересчёт статистики `raw_bytes`, `stored_bytes`, `dedup_saved_bytes`
- локальный Sled-кэш и проверку синхронизации с удалённым `index.enc`
- `meta`-проверку ключа, чтобы выявлять "другой кошелёк" без полного открытия vault
- удаление осиротевших чанков и GC через `delete_chunk()`

## Тесты

```bash
cargo test
```

Тесты покрывают:

- детерминированную BIP-39/BIP-32 деривацию
- AEAD round-trip и обнаружение подмены данных
- ZSTD compression/decompression
- корректную сериализацию и десериализацию `VaultIndex`
- обработку transient errors в StorageProvider

