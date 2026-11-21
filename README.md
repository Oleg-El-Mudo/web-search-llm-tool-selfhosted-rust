# sh_web_search_tool_rs

Локальный сервис поиска по DuckDuckGo с извлечением содержимого найденных страниц.
Один бинарник, один процесс, два транспорта:

- **HTTP** (Axum, порт `3333`) — для HTTP-бэкендов и харнессов (Cherry Studio и т.п.);
- **MCP** (Model Context Protocol, stdio) — для LLM-агентов (OpenCode, Claude Desktop, Cherry Studio), которые сами запускают бинарник как дочерний процесс.

Общая логика поиска (`src/search.rs`) у обоих транспортов одна.

## Возможности

- Поиск по DuckDuckGo (HTML-версия, без JavaScript), без API-ключей.
- Извлечение читаемого контента из найденных страниц (`fetch_content`).
- Двухэтапный обход `403`: первый запрос ловит `Set-Cookie`, второй повторяет запрос с полученными куками.
- Обход страницы аномалии DuckDuckGo через извлечение и подстановку `vqd`-токена.
- Retry при таймауте подключения к DuckDuckGo.
- Эвристики «шума»: оглавления, навигация и футеры отфильтровываются при извлечении контента.
- Заголовки браузера: `Referer`, `Cookie`, `Sec-CH-UA-*`, `Sec-Fetch-*` — для обхода базовой защиты от ботов.
- HTTP и MCP в одном процессе, конкурентно, с корректным завершением по `SIGINT`/`SIGTERM`.
- Полностью статический бинарник (musl, static-pie): TLS через rustls, OpenSSL не нужен.

## Архитектура

```
        MCP client (OpenCode, Claude, inspector)      HTTP client (curl, Cherry Studio, harness)
                 | spawn as child process                            |
                 v                                                  v
        +---------------------------------------------------------------+
        |            sh_web_search_tool_rs  (один процесс)            |
        |                                                             |
        |   src/mcp_server.rs            src/http_server.rs          |
        |   tool: web_search (stdio)     POST/GET /search, /health   |
        |            \                        /                       |
        |             +---- src/search.rs ----+                      |
        |        web_search(query, max_results, fetch_content)       |
        |        + Markdown-рендер для LLM                           |
        |                    |                                        |
        |              reqwest (rustls)                              |
        +-------------------------------|----------------------------+
                                        v
                     +------------------+------------------+
                     | DuckDuckGo HTML  |  целевые сайты  |
                     | (поиск + vqd)    | (fetch + parser)|
                     +------------------+------------------+

Shutdown: SIGINT/SIGTERM -> CancellationToken -> axum graceful shutdown
                         + rmcp RunningService cancel -> exit 0
Логи пишутся в stderr: stdout занят JSON-RPC-потоком MCP.
```

### Модули

| Файл | Назначение |
| --- | --- |
| `src/main.rs` | Точка входа: разбор флагов/ENV, конкурентный запуск обоих серверов, graceful shutdown |
| `src/search.rs` | Ядро: асинхронный `web_search(query, max_results, fetch_content) -> Vec<SearchResult>`, парсинг DDG, извлечение контента, Markdown-рендер |
| `src/http_server.rs` | Axum-роуты и HTTP-обработчики (тонкая прослойка над `search`) |
| `src/mcp_server.rs` | MCP-сервер (`rmcp`, stdio), регистрация инструмента `web_search` |

## Быстрый старт

Требуется Rust 1.88+ (https://rustup.rs/).

```sh
make build                      # cargo build --release
./target/release/sh_web_search_tool_rs
```

По умолчанию поднимаются оба сервера: HTTP на `0.0.0.0:3333` и MCP на stdio.

```sh
# HTTP
curl -s localhost:3333/health
curl -s -X POST localhost:3333/search \
  -H 'content-type: application/json' \
  -d '{"query":"rust tokio tutorial","max_results":3,"fetch_content":true}'

# HTTP (GET — удобно для ручной проверки)
curl -s "localhost:3333/search?query=rust%20tokio&max_results=2&fetch_content=false"

# только HTTP
./target/release/sh_web_search_tool_rs --http-only

# только MCP (для MCP-клиентов)
./target/release/sh_web_search_tool_rs --mcp-only

# другой порт / хост
WEBSEARCH_PORT=8080 WEBSEARCH_HOST=127.0.0.1 ./target/release/sh_web_search_tool_rs --http-only
```

Остановить: `Ctrl+C` (или `SIGTERM`) — серверы завершаются корректно, код выхода `0`.

### Флаги и переменные окружения

| Параметр | Значение |
| --- | --- |
| `--http-only` | только HTTP-сервер |
| `--mcp-only` | только MCP-сервер (stdio) |
| `-h`, `--help` | справка |
| `WEBSEARCH_HOST` | адрес HTTP-бинда, по умолчанию `0.0.0.0` |
| `WEBSEARCH_PORT` / `PORT` | порт HTTP, по умолчанию `3333` |
| `RUST_LOG` | уровень/фильтр логов, по умолчанию `info` |

## HTTP API

`POST /search`, тело JSON:

```json
{ "query": "rust tokio tutorial", "max_results": 3, "fetch_content": true }
```

| Поле | Тип | По умолчанию | Описание |
| --- | --- | --- | --- |
| `query` | string | обязателен | поисковый запрос |
| `max_results` | integer | `3` | число результатов (ограничивается диапазоном 1–20) |
| `fetch_content` | boolean | `false` | скачивать и извлекать содержимое каждой страницы |

Ответ — JSON-массив:

```json
[
  {
    "title": "Tutorial | Tokio - An asynchronous Rust runtime",
    "url": "https://tokio.rs/tokio/tutorial",
    "snippet": "Learn how to write asynchronous applications with Tokio...",
    "content": "извлечённый текст страницы (пусто, если fetch_content=false)"
  }
]
```

Замечание: по HTTP `fetch_content` по умолчанию `false` (историческое поведение API), по MCP — `true` (удобно для LLM).

Ошибки: `502` с телом `{"error": "..."}` (DuckDuckGo недоступен, страница аномалии, сетевая ошибка).

`GET /health` → `ok`.

## MCP: инструмент `web_search`

Схема инструмента (реальный вывод `tools/list`):

```json
{
  "name": "web_search",
  "description": "Search the web (DuckDuckGo) and return the found pages with title, URL, snippet and extracted content as Markdown",
  "inputSchema": {
    "type": "object",
    "properties": {
      "query":         { "type": "string",  "description": "Search query" },
      "max_results":   { "type": "integer", "default": 3,    "description": "Maximum number of results to return (default: 3)" },
      "fetch_content": { "type": "boolean", "default": true, "description": "Fetch full page content for every hit (default: true)" }
    },
    "required": ["query"]
  }
}
```

Формат ответа для LLM — Markdown с разделителями:

```markdown
# Web search: rust tokio tutorial

Found 2 result(s):

---

## 1. Tutorial | Tokio - An asynchronous Rust runtime

**URL:** https://tokio.rs/tokio/tutorial

**Snippet:** Learn how to write asynchronous applications with Tokio...

**Content:**

…извлечённый текст страницы, либо «(контент недоступен)»…

---
```

Содержимое страницы ограничивается 6000 символами (метка `… (content truncated)`), чтобы не раздувать контекст модели. При пустом результате возвращается `isError: true` с текстом `No results found for '<query>'`.

Ручная проверка MCP-транспорта:

```sh
printf '%s\n' \
 '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"cli","version":"1"}}}' \
 '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
 '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
 '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"web_search","arguments":{"query":"musl static rust","max_results":2,"fetch_content":false}}}' \
 | ./target/release/sh_web_search_tool_rs --mcp-only 2>/dev/null

# или через официальный инспектор
npx @modelcontextprotocol/inspector --cli ./target/release/sh_web_search_tool_rs --mcp-only --method tools/list
```

## Установка в production

### Статический бинарник

```sh
rustup target add x86_64-unknown-linux-musl
make build-static          # static-pie, без внешних зависимостей
file target/x86_64-unknown-linux-musl/release/sh_web_search_tool_rs
# ... ELF 64-bit LSB pie executable ... static-pie linked, stripped
```

Такой бинарник копируется на любой x86_64 Linux-сервер и не требует ни glibc-зависимостей, ни OpenSSL (TLS — `rustls`). `make install` предпочитает именно статическую сборку, если она существует.

### `install.sh` + systemd

```sh
make install               # build + sudo ./install.sh install
```

`install.sh install` делает:

1. выбирает бинарник (предпочитает статический musl, иначе `target/release`);
2. создаёт системного пользователя `websearch` (`nologin`, без home);
3. кладёт бинарник в `/usr/local/bin/web-search`, unit — в `/etc/systemd/system/web-search.service`, README — в `/usr/local/share/doc/web-search/`;
4. `systemctl daemon-reload && systemctl enable web-search && systemctl restart web-search`.

Удаление:

```sh
make uninstall             # sudo ./install.sh uninstall
```

`web-search.service`: автозапуск (`WantedBy=multi-user.target`, `enable`), запуск от `websearch`, логи в journald (`SyslogIdentifier=web-search`), `Restart=always` + `RestartSec=3`, жёсткая песочница (`ProtectSystem=strict`, `NoNewPrivileges`, `PrivateTmp`, без capabilities). Внутри unit используется `--http-only`: stdio-MCP в демоне не имеет смысла, MCP запускается MCP-клиентом.

Сервис слушает `127.0.0.1:3333`. Для доступа из сети поправьте `WEBSEARCH_HOST=0.0.0.0` в unit-файле и перезапустите сервис.

Проверка:

```sh
systemctl status web-search
journalctl -u web-search -f
curl -s localhost:3333/health
```

## Интеграция с OpenCode (MCP)

Конфигурация проекта — `opencode.jsonc` в корне репозитория (или глобально в `~/.config/opencode/opencode.jsonc`):

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
      "web-search": {
        "type": "local",
        "command": ["/usr/local/bin/web-search", "--mcp-only"],
        "cwd": "~",
        "environment": {
          "RUST_LOG": "info"
        },
        "codemode": false
      }
    }
  }
}
```

`--mcp-only` обязателен: OpenCode сам запускает бинарник как дочерний процесс и общается по stdio, лишний HTTP-бинд только создаёт конфликт портов.

То же самое одной командой:

```sh
opencode mcp add web-search --env RUST_LOG=info -- /usr/local/bin/web-search --mcp-only
opencode mcp list          # ✓ web-search  connected
```

Если OpenCode работает в режиме Code Mode (по умолчанию), доступ к инструменту выглядит так:

```js
await tools.web_search.web_search({ query: "rust tokio tutorial", max_results: 3, fetch_content: true })
```

при `codemode: false` инструмент появляется в списке инструментов модели как `web_search_web_search` (`<сервер>_<инструмент>`).

Локальная сборка вместо установленной в `/usr/local/bin`:

```jsonc
{
  "mcp": {
    "servers": {
      "web-search": {
        "type": "local",
        "command": ["/home/you/projects/sh_web_search_tool_rs/target/release/sh_web_search_tool_rs", "--mcp-only"]
      }
    }
  }
}
```

MCP-подключение к другим клиентам (Claude Desktop и подобные) — тот же принцип: `command` = путь к бинарнику + `--mcp-only`.

## Устранение неполадок

### Логи сервиса

```sh
journalctl -u web-search -f              # поток логов
journalctl -u web-search --since "10 min ago"
systemctl status web-search -n 50        # последние строки + состояние
```

Уровень логов меняется в unit-файле (`Environment=RUST_LOG=debug`) после `systemctl daemon-reload && systemctl restart web-search`. Логи всегда идут в **stderr** — stdout занят MCP-потоком.

### Проверка живости HTTP

```sh
curl -s -i localhost:3333/health
curl -s -i -X POST localhost:3333/search -H 'content-type: application/json' \
  -d '{"query":"test","max_results":1,"fetch_content":false}'
```

- `200` в `/health` — процесс жив.
- `502 {"error":"DuckDuckGo blocked the request (anomaly page)..."}` — DuckDuckGo, см. ниже.
- `connection refused` — сервис не поднят или слушает другой адрес/порт: проверьте `WEBSEARCH_HOST`/`WEBSEARCH_PORT` в `systemctl cat web-search`.

### Проблемы с DuckDuckGo

| Симптом в логах | Причина | Что делать |
| --- | --- | --- |
| `Anomaly page detected, retrying with vqd` затем `Search failed: DuckDuckGo blocked...` | антибот-страница, `vqd`-токен не извлечён или не принят | подождать 30–60 с, уменьшить число запросов, снизить `max_results` |
| `Found 0 result bodies` | DuckDuckGo отдал страницу без результатов (рейт-лимит/пустая выдача) | повторить запрос, упростить `query` |
| `DDG first attempt failed ... retrying...` | таймаут/сетевая ошибка | проверить исходящий трафик и DNS с сервера |
| `"content": ""` или `(контент недоступен)` | сайт отдал `403`, слишком короткий HTML или «шум» вместо текста | норма для части сайтов; попробуйте `fetch_content: false` и сниппет |
| `Fetch 403 for <url>` | сайт требует cookies/JS | сервис уже пытается повтор с `Set-Cookie`; если не помогает — контент недоступен по существу |

Совет для диагностики: `RUST_LOG=debug` и повтор запроса через `curl` — видно и статус DuckDuckGo, и причину отказа конкретного сайта.

### Порт занят

```
WARN HTTP server unavailable: Address already in use (MCP keeps running)
```

В смешанном режиме (HTTP + MCP) конфликт порта не убивает процесс: MCP продолжает работать. Для демона используйте `--http-only` и другой порт (`WEBSEARCH_PORT=8080`).

### MCP-клиент не подключается

- Запускайте бинарник с `--mcp-only` (см. раздел про OpenCode).
- Не печатайте в stdout: всё stdout — только JSON-RPC. Свои сообщения пишите в stderr.
- Проверьте вручную через `npx @modelcontextprotocol/inspector --cli <binary> --mcp-only --method tools/list`.
- `Ctrl+C`/закрытие клиента приводит к `MCP server stopped: Closed` и коду выхода `0`.

### Статическая сборка не получается

- `missing target: run 'rustup target add x86_64-unknown-linux-musl'` — выполните команду.
- `failed to find tool "x86_64-linux-musl-gcc"` — `ring` (TLS) требует C-компилятор для musl: установите `musl-tools` (Debian/Ubuntu) или `musl-gcc`; `make build-static` сам подхватит `x86_64-linux-musl-gcc`/`musl-gcc`, иначе использует системный `cc`.
- `-C target-feature=+crt-static` на `x86_64-unknown-linux-gnu` ломает proc-macro-крейты (`serde_derive`, `async-trait`, `rmcp-macros`) — статика включается только для musl-таргета (см. `.cargo/config.toml`).

## Разработка

```sh
make test        # unit-тесты (парсинг DDG-ссылок, Markdown-рендер, обрезка контента)
make check       # fmt + clippy (-D warnings) + тесты
make run         # build + запуск обоих серверов
make clean
```

Зависимости: `axum`, `reqwest` (rustls), `rmcp` (официальный Rust SDK для MCP), `scraper`, `html2text`, `tokio`, `tracing`.
