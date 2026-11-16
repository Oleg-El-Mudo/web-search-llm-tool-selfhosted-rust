# sh_web_search_tool_rs

Локальный HTTP-сервис поиска по DuckDuckGo с извлечением контента из найденных страниц. Предназначен для использования как HTTP-бэкенд в LLM harness (включая Cherry Studio) для обеспечения моделей доступом к актуальной информации из интернета.

## Возможности

- Поиск по DuckDuckGo (HTML-версия, без JavaScript).
- Извлечение читаемого контента из найденных страниц (опционально, `fetch_content`).
- Двухэтапный обход 403: первый запрос ловит `Set-Cookie` с целевого сайта, второй — повторяет запрос с полученными куками.
- Обход страницы аномалии DuckDuckGo через извлечение и подстановку `vqd`-токена.
- Автоматический retry при таймауте подключения.
- Эвристики "шума": распознаёт оглавления, навигацию и футеры, отфильтровывает их при извлечении контента.
- Заголовки браузера: `Referer`, `Cookie`, `Sec-CH-UA-*`, `Sec-Fetch-*` — для обхода базовой защиты от ботов.

## Архитектура

```
Client (harness)
    |
    | POST /search
    v
+------------------+
|   Rust Server     |
|   (Axum + Tokio)  |
+------------------+
    |
    | GET (с заголовками браузера)
    v
+------------------+     +------------------+
| DuckDuckGo HTML  | --> |   Target site    |
| (поиск + vqd)    |     | (fetch + парсер) |
+------------------+     +------------------+
```

## Установка

### Требования

- Rust toolchain (версия 1.78+). Установить можно с https://rustup.rs/:
  ```
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  source $HOME/.cargo/env
  ```

### Сборка

```bash
git clone git@gitverse.ru:Fishman/web-search-llm-tool-selfhosted-rust.git 
cd sh_web_search_tool_rs
cargo build --release
```

Бинарный файл появится в `target/release/sh_web_search_tool_rs`.

### Запуск

```bash
./target/release/sh_web_search_tool_rs
```

Сервер слушает `0.0.0.0:3333`. В логах появится строка:

```
Rust Search Server running on port 3333
```

## API

Все запросы — POST на `/search` с заголовком `Content-Type: application/json`.

### Тело запроса

| Поле | Тип | По умолчанию | Описание |
|---|---|---|---|
| `query` | string | обязателен | Поисковый запрос |
| `max_results` | integer | 3 | Максимальное число результатов |
| `fetch_content` | boolean | false | Скачивать и извлекать контент страниц |

### Тело ответа

Массив объектов:

```json
[
  {
    "title": "Заголовок страницы",
    "url": "https://example.com/article",
    "snippet": "Краткое описание из поисковой выдачи",
    "content": "Текст статьи, извлечённый из HTML"
  }
]
```

Если `fetch_content: false`, поле `content` пустое. Если сервер не смог получить страницу (403, таймаут), `content` тоже пустое — модель не путают сообщения об ошибках.

## Примеры вызова

### Пример 1. Базовый поиск, без контента

```bash
curl -s -X POST \
  -H "Content-Type: application/json" \
  -d '{
    "query": "основы Rust язык программирования",
    "max_results": 3,
    "fetch_content": false
  }' \
  http://localhost:3333/search
```

Ответ:

```json
[
  {
    "title": "Руководство по языку программирования Rust",
    "url": "https://metanit.com/rust/tutorial/",
    "snippet": "Глава 1. Введение в язык Rust. Общий обзор. Установка. Первая программа. Глава 2. Основы Rust.",
    "content": ""
  },
  {
    "title": "Обучение Rust с нуля - бесплатный онлайн-курс | Coddy",
    "url": "https://coddy.tech/landing/ru/rust",
    "snippet": "Изучайте Rust бесплатно: интерактивные уроки, подсказки ИИ и сертификат.",
    "content": ""
  }
]
```

### Пример 2. Поиск с извлечением контента

```bash
curl -s -X POST \
  -H "Content-Type: application/json" \
  -d '{
    "query": "Rust ownership borrowing tutorial",
    "max_results": 2,
    "fetch_content": true
  }' \
  http://localhost:3333/search
```

Ответ — массив из 2 объектов, каждый с полным текстом статьи в поле `content`. Если какой-то сайт вернул 403 — `content` пустой, но `snippet` и `title` есть — модель может оценить релевантность по ним и перейти по `url` самостоятельно.

### Пример 3. Подключение из Python-скрипта

```python
import requests
import json

def search(query, max_results=3, fetch_content=True):
    resp = requests.post(
        "http://localhost:3333/search",
        json={
            "query": query,
            "max_results": max_results,
            "fetch_content": fetch_content,
        },
    )
    return resp.json()

results = search("Rust async await", max_results=3)
for r in results:
    print(f"[{r['title']}]({r['url']})")
    if r["content"]:
        print(r["content"][:500])
        print("---")
```

### Пример 4. Использование в LLM-контексте (prompt-шаблон)

```python
results = search("difference between C++ and Rust memory management", fetch_content=True)

prompt_template = """
User asked about: {query}

Here are relevant search results:

{results}

Answer the user's question using the information above.
"""

results_text = ""
for i, r in enumerate(results, 1):
    content = r.get("content", "(content unavailable)")
    results_text += f"--- Result {i}: {r['title']} ---\n"
    results_text += f"URL: {r['url']}\n"
    results_text += f"Snippet: {r['snippet']}\n"
    results_text += f"Content (first 2000 chars):\n{content[:2000]}\n\n"

prompt = prompt_template.format(query=query, results=results_text)
```

### Пример 5. Поиск по конкретной теме — "дженерики в Rust"

```bash
curl -s -X POST \
  -H "Content-Type: application/json" \
  -d '{
    "query": "generics traits Rust programming",
    "max_results": 5,
    "fetch_content": true
  }' \
  http://localhost:3333/search
```

### Пример 6. Поиск на русском языке

```bash
curl -s -X POST \
  -H "Content-Type: application/json" \
  -d '{
    "query": "как работает borrow checker в Rust",
    "max_results": 3,
    "fetch_content": true
  }' \
  http://localhost:3333/search
```

## Настройка в качестве инструмента web search

### Cherry Studio — HTTP tool

Добавьте HTTP-инструмент с параметрами:

| Параметр | Значение |
|---|---|
| URL | `http://localhost:3333/search` |
| Method | `POST` |
| Content-Type | `application/json` |
| Body | `{"query": "{{query}}", "max_results": 3, "fetch_content": true}` |

### Прямой вызов из кода (JavaScript/TypeScript)

```typescript
async function search(query: string, maxResults = 3): Promise<SearchResult[]> {
  const resp = await fetch("http://localhost:3333/search", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ query, max_results: maxResults, fetch_content: true }),
  });
  return resp.json();
}

// Использование
const results = await search("Rust vs Zig performance");
for (const r of results) {
  console.log(r.title, r.content.length > 0 ? "(has content)" : "(snippet only)");
}
```

### Прямой вызов из Go

```go
package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"net/http"
)

type SearchRequest struct {
	Query        string `json:"query"`
	MaxResults   int    `json:"max_results"`
	FetchContent bool   `json:"fetch_content"`
}

type SearchResult struct {
	Title   string `json:"title"`
	URL     string `json:"url"`
	Snippet string `json:"snippet"`
	Content string `json:"content"`
}

func Search(query string) ([]SearchResult, error) {
	reqBody := SearchRequest{
		Query:        query,
		MaxResults:   3,
		FetchContent: true,
	}
	body, _ := json.Marshal(reqBody)
	resp, err := http.Post("http://localhost:3333/search", "application/json", bytes.NewBuffer(body))
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	var results []SearchResult
	json.NewDecoder(resp.Body).Decode(&results)
	return results, nil
}
```

## Структура проекта

```
sh_web_search_tool_rs/
  Cargo.toml          -- зависимости: axum, reqwest, tokio, scraper, html2text и др.
  LICENSE             -- GPLv3
  README.md
  src/
    main.rs           -- весь код в одном файле (~550 строк)
```

## Отладка

Сервер пишет логи в stderr:

```
INFO sh_web_search_tool_rs: Search: query='osnovy rust', max_results=2, fetch_content=true
INFO sh_web_search_tool_rs: DDG response length=34250
INFO sh_web_search_tool_rs: Found 10 result bodies
INFO sh_web_search_tool_rs: Fetch 403 for https://coddy.tech/landing/ru/rust: 403 Forbidden
```

Ключевые строки:
- `Search: query=...` — входящий запрос
- `DDG response length=` — успешный ответ от DuckDuckGo
- `Found N result bodies` — сколько результатов найдено
- `Fetch STATUS for URL: STATUS` — результат фетча каждой страницы

Для более детальных логов:

```bash
RUST_LOG=debug ./target/release/sh_web_search_tool_rs
```

## Известные проблемы и план развития

### Текущие ограничения

- **metanit.com** — при фетче контента возвращается index-страница (оглавление всего туториала) вместо первой главы. Эвристика `looks_like_noise` пропускает её, так как она содержит текст и заголовки.
- **coddy.tech** — отдаёт 403 даже после двухэтапного обхода с Cookie. Вероятно, используется глубокая проверка User-Agent (UA fingerprinting) на уровне Cloudflare или AWS WAF.
- **ru.wikipedia.org** — требует заголовка `Referer` с домена wikipedia.org, иначе отдаёт 403. Текущий `Referer: https://duckduckgo.com/` не работает.
- **Оглавление вместо статьи** — у некоторых сайтов (например rust.codeutility.io) селектор `main` может вернуть страницу навигации/оглавления вместо статьи.
- **Таймауты** — при медленном соединении DDG может не отвечать за 25 секунд, и retry не помогает. В этом случае возвращается пустой массив результатов.

### Что можно улучшить

- **Глубокая навигация по статьям**: если первый ответ — оглавление, автоматически фетчить первую главу или раздел, соответствующий запросу.
- **Сбор реальных Cookie**: вместо заглушки `sess=abc123` — использовать реальные сессии или `http-cookie`-фреймворк для управления куками между запросами.
- **Wikipedia-specific headers**: для домена `*.wikipedia.org` добавлять `Referer: https://ru.wikipedia.org/` и `Accept-Language: ru-RU,ru;q=0.9,en-US;q=0.7,en;q=0.6`.
- **UA-фingerprinting для Cloudflare**: добавить заголовки `Sec-CH-UA-Prefers-Color-Scheme`, `Sec-CH-UA-Mobile`, `Sec-CH-UA-Arch`, `Sec-CH-UA-Bitness` в правильном порядке и с актуальными значениями.
- **Прокси-поддержка**: добавить параметр `proxy` в запрос для обхода геоблокировки и ограничения частоты запросов.
- **Кэширование результатов**: сохранять кэш ответов DuckDuckGo на N секунд, чтобы одинаковые запросы не шли дважды.
- **Параллельный фетч контента**: текущая реализация фетчит каждую страницу последовательно — можно сделать параллельно через `tokio::spawn`.
- **Адаптивные таймауты**: вместо статических 25 секунд — динамический таймаут на основе предыдущих ответов (например, алгоритм с exponential backoff).
- **Поддержка Bing/Yandex**: переключение между источниками через параметр `search_engine` в запросе.
- **Docker-образ**: добавить `Dockerfile` для удобства развёртывания.
