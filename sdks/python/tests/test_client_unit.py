"""Unit tests for CrwClient — all network/subprocess calls are mocked."""

from __future__ import annotations

import json
from unittest.mock import MagicMock, patch

import pytest

from crw.client import CLOUD_API_URL, CrwClient, _local_child_env
from crw.exceptions import CrwApiError, CrwError, CrwExtractCancelledError, CrwTimeoutError


@pytest.fixture(autouse=True)
def _clean_crw_env(monkeypatch: pytest.MonkeyPatch) -> None:
    """Hermetic env: each test opts into local (CRW_LOCAL) or cloud explicitly."""
    for key in ("CRW_LOCAL", "CRW_API_KEY", "CRW_API_URL", "CRW_BINARY"):
        monkeypatch.delenv(key, raising=False)


def _local_client(monkeypatch: pytest.MonkeyPatch) -> CrwClient:
    """A zero-config local (subprocess) client — the CRW_LOCAL opt-in."""
    monkeypatch.setenv("CRW_LOCAL", "1")
    return CrwClient()


# ---------------------------------------------------------------------------
# Init mode detection (cloud-first)
# ---------------------------------------------------------------------------


@pytest.mark.unit
class TestInit:
    def test_default_cloud_requires_key(self) -> None:
        # Cloud-first: no key, no CRW_LOCAL → friendly onboarding nudge.
        with pytest.raises(CrwError, match="1000 free credits"):
            CrwClient()

    def test_default_cloud_with_key(self) -> None:
        client = CrwClient(api_key="crw_live_test")
        assert client._api_url == CLOUD_API_URL
        assert client._api_key == "crw_live_test"

    def test_local_opt_in_via_env(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("CRW_LOCAL", "1")
        client = CrwClient()
        assert client._api_url is None  # subprocess/local mode

    def test_explicit_self_host_no_key(self) -> None:
        # An explicit server URL does not require a key (self-host without auth).
        client = CrwClient(api_url="http://localhost:3000")
        assert client._api_url == "http://localhost:3000"

    def test_key_from_env(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("CRW_API_KEY", "env-key")
        client = CrwClient()
        assert client._api_key == "env-key"
        assert client._api_url == CLOUD_API_URL


# ---------------------------------------------------------------------------
# Scrape
# ---------------------------------------------------------------------------


@pytest.mark.unit
class TestScrape:
    def test_scrape_http_builds_correct_request(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api", api_key="crw_live_test")
        mock_response = {"markdown": "# Hello", "metadata": {"title": "Hello"}}

        with patch.object(client, "_http_post", return_value=mock_response) as mock_post:
            result = client.scrape("https://example.com", formats=["markdown", "html"])

        mock_post.assert_called_once()
        call_args = mock_post.call_args
        assert call_args[0][0] == "/v1/scrape"
        body = call_args[0][1]
        assert body["url"] == "https://example.com"
        assert body["formats"] == ["markdown", "html"]
        assert "onlyMainContent" not in body  # server default applies
        assert result == mock_response

    def test_scrape_sends_only_main_content_when_set(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api", api_key="crw_live_test")
        with patch.object(client, "_http_post", return_value={}) as mock_post:
            client.scrape("https://example.com", only_main_content=False)
        assert mock_post.call_args[0][1]["onlyMainContent"] is False

    def test_scrape_subprocess_calls_tool(self, monkeypatch: pytest.MonkeyPatch) -> None:
        client = _local_client(monkeypatch)
        mock_response = {"markdown": "# Hello"}

        with patch.object(client, "_tool_call", return_value=mock_response) as mock_tool:
            result = client.scrape("https://example.com")

        mock_tool.assert_called_once()
        call_args = mock_tool.call_args
        assert call_args[0][0] == "crw_scrape"
        assert call_args[0][1]["url"] == "https://example.com"
        assert result == mock_response


# ---------------------------------------------------------------------------
# Crawl
# ---------------------------------------------------------------------------


@pytest.mark.unit
class TestCrawl:
    def test_crawl_http_polls_until_complete(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api", api_key="crw_live_test")

        crawl_start_response = {"id": "job-123"}
        poll_in_progress = {"success": True, "status": "scraping", "data": []}
        poll_completed = {
            "success": True,
            "status": "completed",
            "data": [{"url": "https://example.com", "markdown": "# Hello"}],
        }

        with (
            patch.object(client, "_http_post", return_value=crawl_start_response),
            patch.object(
                client, "_http_request", side_effect=[poll_in_progress, poll_completed]
            ) as mock_req,
            patch("crw.client.time.sleep"),
        ):
            result = client.crawl("https://example.com", poll_interval=0.01, timeout=10)

        assert mock_req.call_count == 2
        assert len(result) == 1
        assert result[0]["url"] == "https://example.com"

    def test_crawl_raises_timeout(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api", api_key="crw_live_test")

        crawl_start_response = {"id": "job-timeout"}
        poll_in_progress = {"success": True, "status": "scraping", "data": []}

        with (
            patch.object(client, "_http_post", return_value=crawl_start_response),
            patch.object(client, "_http_request", return_value=poll_in_progress),
            patch("crw.client.time.sleep"),
            patch("crw.client.time.monotonic", side_effect=[0.0, 0.0, 100.0]),
        ):
            with pytest.raises(CrwTimeoutError, match="timed out"):
                client.crawl("https://example.com", poll_interval=0.01, timeout=5)

    def test_crawl_raises_on_failure(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api", api_key="crw_live_test")

        crawl_start_response = {"id": "job-fail"}
        poll_failed = {"success": True, "status": "failed", "error": "Something went wrong"}

        with (
            patch.object(client, "_http_post", return_value=crawl_start_response),
            patch.object(client, "_http_request", return_value=poll_failed),
            patch("crw.client.time.sleep"),
        ):
            with pytest.raises(CrwError, match="Crawl failed"):
                client.crawl("https://example.com", poll_interval=0.01, timeout=10)


# ---------------------------------------------------------------------------
# Map
# ---------------------------------------------------------------------------


@pytest.mark.unit
class TestLocalChildEnv:
    def test_crw_api_url_is_not_inherited(self, monkeypatch: pytest.MonkeyPatch) -> None:
        # crw-mcp binds CRW_API_URL itself and proxies to the cloud when it is
        # set, so an inherited one turned CRW_LOCAL into a billed cloud call
        # answering with the REST envelope instead of the flat payload.
        monkeypatch.setenv("CRW_API_URL", "https://api.fastcrw.com")
        monkeypatch.setenv("CRW_CLIENT__API_URL", "https://api.fastcrw.com")
        monkeypatch.setenv("CRW_API_KEY", "crw_live_test")
        env = _local_child_env()
        assert "CRW_API_URL" not in env
        assert "CRW_CLIENT__API_URL" not in env
        # The key is harmless: an embedded child ignores it.
        assert env["CRW_API_KEY"] == "crw_live_test"


@pytest.mark.unit
class TestMap:
    def test_map_http_returns_links(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api", api_key="crw_live_test")
        mock_response = {"links": ["https://example.com/a", "https://example.com/b"]}

        with patch.object(client, "_http_post", return_value=mock_response):
            result = client.map("https://example.com")

        assert result == ["https://example.com/a", "https://example.com/b"]

    def test_map_http_exposes_sitemaps(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api", api_key="crw_live_test")
        mock_response = {
            "links": ["https://example.com/a"],
            "sitemaps": ["https://example.com/sitemap.xml"],
        }

        with patch.object(client, "_http_post", return_value=mock_response):
            result = client.map("https://example.com")

        assert result.sitemaps == ["https://example.com/sitemap.xml"]

    def test_map_missing_sitemaps_is_an_empty_list(self) -> None:
        # An engine old enough not to send the key must not surface None, so
        # len(result.sitemaps) is always safe.
        client = CrwClient(api_url="https://fastcrw.com/api", api_key="crw_live_test")

        with patch.object(client, "_http_post", return_value={"links": []}):
            result = client.map("https://example.com")

        assert result.sitemaps == []

    def test_map_result_still_behaves_as_a_plain_list(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api", api_key="crw_live_test")
        mock_response = {"links": ["https://example.com/a"], "sitemaps": ["https://s.xml"]}

        with patch.object(client, "_http_post", return_value=mock_response):
            result = client.map("https://example.com")

        assert isinstance(result, list)
        assert result == ["https://example.com/a"]
        assert len(result) == 1
        assert json.dumps(result) == json.dumps(["https://example.com/a"])

    def test_map_malformed_body_yields_empty_lists(self) -> None:
        # A self-hosted server or gateway can answer anything; neither field may
        # raise out of the SDK nor turn a string into a list of characters.
        client = CrwClient(api_url="https://fastcrw.com/api", api_key="crw_live_test")

        with patch.object(client, "_http_post", return_value={"links": "none", "sitemaps": 3}):
            result = client.map("https://example.com")

        assert result == []
        assert result.sitemaps == []

        with patch.object(client, "_http_post", return_value={"links": None}):
            assert client.map("https://example.com") == []

    def test_map_local_flat_shape(self, monkeypatch: pytest.MonkeyPatch) -> None:
        # Embedded crw-mcp answers flat.
        client = _local_client(monkeypatch)
        mock_response = {
            "success": True,
            "links": ["https://example.com/a"],
            "sitemaps": ["https://example.com/sitemap.xml"],
        }

        with patch.object(client, "_tool_call", return_value=mock_response):
            result = client.map("https://example.com")

        assert result == ["https://example.com/a"]
        assert result.sitemaps == ["https://example.com/sitemap.xml"]

    def test_map_local_proxy_envelope(self, monkeypatch: pytest.MonkeyPatch) -> None:
        # A crw-mcp that inherited CRW_API_URL proxies and answers with the REST
        # envelope. Before the unwrap this returned [] and dropped sitemaps.
        client = _local_client(monkeypatch)
        mock_response = {
            "success": True,
            "data": {
                "links": ["https://example.com/a"],
                "sitemaps": ["https://example.com/sitemap.xml"],
            },
            "creditCost": 1,
        }

        with patch.object(client, "_tool_call", return_value=mock_response):
            result = client.map("https://example.com")

        assert result == ["https://example.com/a"]
        assert result.sitemaps == ["https://example.com/sitemap.xml"]

    def test_map_local_ignores_an_unrelated_top_level_data(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        # The probe is `data.links`, not a bare `data`: a flat response that
        # grows some other top-level `data` must not be unwrapped into.
        client = _local_client(monkeypatch)
        mock_response = {
            "success": True,
            "links": ["https://example.com/a"],
            "sitemaps": ["https://example.com/sitemap.xml"],
            "data": {"note": 1},
        }

        with patch.object(client, "_tool_call", return_value=mock_response):
            result = client.map("https://example.com")

        assert result == ["https://example.com/a"]
        assert result.sitemaps == ["https://example.com/sitemap.xml"]

    def test_map_local_null_data_does_not_raise(self, monkeypatch: pytest.MonkeyPatch) -> None:
        # A third-party gateway may answer {"data": null}; unwrapping blindly
        # would raise AttributeError here.
        client = _local_client(monkeypatch)

        with patch.object(client, "_tool_call", return_value={"success": True, "data": None}):
            result = client.map("https://example.com")

        assert result == []
        assert result.sitemaps == []


# ---------------------------------------------------------------------------
# Search
# ---------------------------------------------------------------------------


@pytest.mark.unit
class TestSearch:
    def test_search_subprocess_uses_tool(self, monkeypatch: pytest.MonkeyPatch) -> None:
        # search now works in local mode too (routes to the crw_search MCP
        # tool); the engine itself returns a clear error if SearXNG is unset.
        client = _local_client(monkeypatch)
        mock_response = [{"url": "https://example.com", "title": "Example"}]
        with patch.object(client, "_tool_call", return_value=mock_response) as mock_tool:
            result = client.search("python web scraping", limit=3)
        mock_tool.assert_called_once()
        assert mock_tool.call_args[0][0] == "crw_search"
        assert mock_tool.call_args[0][1]["query"] == "python web scraping"
        assert result == mock_response

    def test_search_http_sends_query(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api", api_key="crw_live_test")
        mock_response = [{"url": "https://example.com", "title": "Example"}]

        with patch.object(
            client,
            "_http_request",
            return_value={"success": True, "data": mock_response},
        ) as mock_request:
            result = client.search("python web scraping", limit=5)

        mock_request.assert_called_once_with(
            "POST",
            "/v1/search",
            {"query": "python web scraping", "limit": 5},
            raw=True,
        )
        assert list(result) == mock_response


# ---------------------------------------------------------------------------
# Close / context manager
# ---------------------------------------------------------------------------


@pytest.mark.unit
class TestLifecycle:
    def test_close_terminates_process(self, monkeypatch: pytest.MonkeyPatch) -> None:
        client = _local_client(monkeypatch)
        mock_proc = MagicMock()
        mock_proc.poll.return_value = None  # process is alive
        mock_proc.wait.return_value = 0
        client._process = mock_proc

        client.close()

        mock_proc.stdin.close.assert_called_once()
        assert client._process is None

    def test_context_manager(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api")
        with patch.object(client, "close") as mock_close:
            with client as c:
                assert c is client
            mock_close.assert_called_once()


# ---------------------------------------------------------------------------
# Error handling
# ---------------------------------------------------------------------------


@pytest.mark.unit
class TestErrors:
    def test_api_error_raised_on_failure(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api", api_key="crw_live_test")

        with patch.object(
            client,
            "_http_request",
            side_effect=CrwApiError("Bad request", status_code=400),
        ):
            with pytest.raises(CrwApiError) as exc_info:
                client.scrape("https://example.com")
            assert exc_info.value.status_code == 400

    def test_jsonrpc_error_handling(self, monkeypatch: pytest.MonkeyPatch) -> None:
        client = _local_client(monkeypatch)
        mock_proc = MagicMock()
        error_response = json.dumps({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": -32600, "message": "Invalid request"},
        })
        mock_proc.stdout.readline.return_value = error_response + "\n"
        mock_proc.poll.return_value = None
        client._process = mock_proc

        with pytest.raises(CrwApiError, match="Invalid request"):
            client._jsonrpc("tools/call", {"name": "crw_scrape", "arguments": {}})


# ---------------------------------------------------------------------------
# Scrape — first-class engine params (renderJs / renderer / waitFor / jsonSchema)
# ---------------------------------------------------------------------------


@pytest.mark.unit
class TestScrapeParams:
    def test_first_class_params_mapped_to_camel_case(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api")
        with patch.object(client, "_http_post", return_value={}) as mock_post:
            client.scrape(
                "https://example.com",
                render_js=True,
                renderer="chrome",
                wait_for=1500,
            )
        body = mock_post.call_args[0][1]
        assert body["renderJs"] is True
        assert body["renderer"] == "chrome"
        assert body["waitFor"] == 1500

    def test_json_schema_adds_json_format(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api")
        schema = {"type": "object", "properties": {"title": {"type": "string"}}}
        with patch.object(client, "_http_post", return_value={}) as mock_post:
            client.scrape("https://example.com", formats=["markdown"], json_schema=schema)
        body = mock_post.call_args[0][1]
        assert body["jsonSchema"] == schema
        assert "json" in body["formats"] and "markdown" in body["formats"]


# ---------------------------------------------------------------------------
# parse_file (PDF / structured extraction) — both modes
# ---------------------------------------------------------------------------


@pytest.mark.unit
class TestParseFile:
    def test_subprocess_base64_and_tool(self, monkeypatch: pytest.MonkeyPatch) -> None:
        client = _local_client(monkeypatch)
        with patch.object(client, "_tool_call", return_value={"markdown": "ok"}) as mock_tool:
            client.parse_file(content=b"%PDF-1.4 data", filename="doc.pdf", formats=["markdown"])
        assert mock_tool.call_args[0][0] == "crw_parse_file"
        args = mock_tool.call_args[0][1]
        assert args["filename"] == "doc.pdf"
        assert "contentBase64" in args and args["formats"] == ["markdown"]

    def test_http_uses_multipart(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api")
        with patch.object(client, "_http_multipart", return_value={"markdown": "ok"}) as mock_mp:
            client.parse_file(content=b"%PDF-1.4 data", filename="doc.pdf", json_schema={"x": 1})
        assert mock_mp.call_args[0][0] == "/v2/parse"
        # body is multipart bytes containing the file and options parts
        assert b"name=\"file\"" in mock_mp.call_args[0][1]
        assert b"name=\"options\"" in mock_mp.call_args[0][1]

    def test_requires_path_or_content(self, monkeypatch: pytest.MonkeyPatch) -> None:
        client = _local_client(monkeypatch)
        with pytest.raises(CrwError, match="path= or content="):
            client.parse_file()


# ---------------------------------------------------------------------------
# HTTP-only methods: extract / batch_scrape / capabilities / change_tracking
# ---------------------------------------------------------------------------


@pytest.mark.unit
class TestHttpOnlyMethods:
    @pytest.mark.parametrize(
        "call",
        [
            lambda c: c.extract(["https://example.com"], prompt="x"),
            lambda c: c.start_extract(["https://example.com"], prompt="x"),
            lambda c: c.get_extract("job-1"),
            lambda c: c.cancel_extract("job-1"),
            lambda c: c.batch_scrape(["https://example.com"]),
            lambda c: c.capabilities(),
            lambda c: c.change_tracking_diff({"markdown": "a"}),
        ],
    )
    def test_guard_in_local_mode(self, call, monkeypatch: pytest.MonkeyPatch) -> None:
        client = _local_client(monkeypatch)
        with pytest.raises(CrwError, match="requires HTTP mode"):
            call(client)

    def test_extract_polls_until_complete(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api")
        start = {"success": True, "id": "job-1"}
        processing = {"success": True, "status": "processing"}
        done = {
            "success": True,
            "status": "completed",
            "results": [
                {"url": "https://example.com", "status": "completed", "data": {"title": "Hi"}}
            ],
        }
        with patch.object(
            client, "_http_request", side_effect=[start, processing, done]
        ) as req:
            with patch("time.sleep"):
                result = client.extract(["https://example.com"], schema={"x": 1})
        assert result == [
            {"url": "https://example.com", "status": "completed", "data": {"title": "Hi"}}
        ]
        # Native route, not the FC-legacy /v2/extract.
        assert req.call_args_list[0][0][:2] == ("POST", "/v1/extract")
        assert req.call_args_list[0].kwargs["headers"] == {"Prefer": "respond-async"}

    def test_batch_scrape_follows_pagination_to_completion(self) -> None:
        # A batch larger than one page must return EVERY document, not just the
        # first ~100 the completed status page carries. The SDK follows `next`.
        client = CrwClient(api_url="https://fastcrw.com/api")
        start = {"success": True, "id": "job-1"}
        page1 = {
            "success": True,
            "status": "completed",
            "data": [{"i": 0}],
            "next": "https://fastcrw.com/api/v2/batch/scrape/job-1?skip=1",
        }
        page2 = {"success": True, "status": "completed", "data": [{"i": 1}], "next": None}
        with patch.object(
            client, "_http_request", side_effect=[start, page1, page2]
        ) as req:
            with patch("time.sleep"):
                result = client.batch_scrape(["a", "b"])
        assert result == [{"i": 0}, {"i": 1}]
        # Page 2 fetched via the cursor's path+query, host stripped.
        assert req.call_args_list[2][0][:2] == ("GET", "/v2/batch/scrape/job-1?skip=1")

    def test_batch_scrape_tolerates_null_data_page(self) -> None:
        # A page may serialize `"data": null`; `list(None)` would crash. The
        # completed job here has no docs yet — must yield [], not raise.
        client = CrwClient(api_url="https://fastcrw.com/api")
        start = {"success": True, "id": "job-1"}
        done = {"success": True, "status": "completed", "data": None, "next": None}
        with patch.object(client, "_http_request", side_effect=[start, done]):
            with patch("time.sleep"):
                assert client.batch_scrape(["a"]) == []

    def test_batch_scrape_raises_on_cancel_without_hanging(self) -> None:
        # A cancelled job never reaches "completed"; the poll must stop, not spin
        # until `timeout`.
        client = CrwClient(api_url="https://fastcrw.com/api")
        start = {"success": True, "id": "job-1"}
        cancelled = {"success": True, "status": "cancelled"}
        with patch.object(
            client, "_http_request", side_effect=[start, cancelled]
        ) as req:
            with patch("time.sleep"):
                with pytest.raises(CrwError, match="cancelled"):
                    client.batch_scrape(["a"])
        assert req.call_count == 2  # start + one status poll, no spinning

    def test_start_extract_prefer_managed_and_self_hosted_fixtures(self) -> None:
        accepted = {"id": "job-1", "status": "processing", "urls": 1}
        for client in (
            CrwClient(api_key="crw_live_test"),
            CrwClient(api_url="http://localhost:3000"),
        ):
            with patch.object(client, "_http_request", return_value=accepted) as req:
                result = client.start_extract(
                    ["https://example.com"], schema={"type": "object"}, basis=True
                )
            assert result["id"] == "job-1"
            assert req.call_args[0][:2] == ("POST", "/v1/extract")
            assert req.call_args.kwargs["headers"] == {"Prefer": "respond-async"}
            assert req.call_args[0][2]["basis"] is True

    def test_extract_preserves_managed_sync_fixture_with_prefer(self) -> None:
        client = CrwClient(api_key="crw_live_test")
        sync = {
            "success": True,
            "results": [
                {"url": "https://example.com", "status": "completed", "data": {"title": "Hi"}}
            ],
        }
        with patch.object(client, "_http_request", return_value=sync) as req:
            result = client.extract(["https://example.com"], prompt="title")
        assert result[0]["status"] == "completed"
        assert req.call_args.kwargs["headers"] == {"Prefer": "respond-async"}

    def test_get_and_cancel_extract_use_canonical_route(self) -> None:
        status = {
            "id": "job/one",
            "status": "cancelled",
            "results": [{"url": "https://example.com", "status": "cancelled"}],
            "expiresAt": "2026-07-14T00:00:00Z",
            "creditsUsed": 0,
            "tokensUsed": 0,
        }
        client = CrwClient(api_url="http://localhost:3000")
        with patch.object(client, "_http_request", return_value=status) as req:
            assert client.get_extract("job/one")["status"] == "cancelled"
            assert client.cancel_extract("job/one")["results"][0]["status"] == "cancelled"
        assert req.call_args_list[0][0][:2] == ("GET", "/v1/extract/job%2Fone")
        assert req.call_args_list[1][0][:2] == ("DELETE", "/v1/extract/job%2Fone")

    def test_extract_cancelling_then_typed_cancelled_error(self) -> None:
        client = CrwClient(api_url="http://localhost:3000")
        accepted = {"id": "job-1", "status": "processing", "urls": 2}
        cancelling = {
            "id": "job-1",
            "status": "cancelling",
            "results": [
                {"url": "https://a.example", "status": "completed", "data": {"title": "A"}},
                {"url": "https://b.example", "status": "processing"},
            ],
            "expiresAt": "2026-07-14T00:00:00Z",
            "creditsUsed": 1,
            "tokensUsed": 9,
        }
        cancelled = {
            **cancelling,
            "status": "cancelled",
            "results": [
                {"url": "https://a.example", "status": "completed", "data": {"title": "A"}},
                {"url": "https://b.example", "status": "cancelled"},
            ],
        }
        with patch.object(client, "_http_request", side_effect=[accepted, cancelling, cancelled]):
            with patch("time.sleep"):
                with pytest.raises(CrwExtractCancelledError) as exc:
                    client.extract(
                        ["https://a.example", "https://b.example"],
                        prompt="x",
                        poll_interval=0,
                    )
        assert exc.value.status["status"] == "cancelled"
        assert exc.value.results[0]["status"] == "completed"

    def test_extract_timeout_best_effort_deletes(self) -> None:
        client = CrwClient(api_url="http://localhost:3000")
        accepted = {"id": "job-1", "status": "processing", "urls": 1}
        cancelled = {
            "id": "job-1",
            "status": "cancelled",
            "results": [{"url": "https://example.com", "status": "cancelled"}],
            "expiresAt": "2026-07-14T00:00:00Z",
            "creditsUsed": 0,
            "tokensUsed": 0,
        }
        with patch.object(client, "_http_request", side_effect=[accepted, cancelled]) as req:
            with pytest.raises(CrwTimeoutError):
                client.extract(["https://example.com"], prompt="x", timeout=-1)
        assert req.call_args_list[-1][0][:2] == ("DELETE", "/v1/extract/job-1")

    def test_batch_scrape_returns_pages(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api")
        start = {"success": True, "id": "batch-1"}
        done = {"status": "completed", "data": [{"markdown": "p1"}]}
        with patch.object(client, "_http_request", side_effect=[start, done]):
            with patch("time.sleep"):
                pages = client.batch_scrape(["https://example.com"])
        assert pages == [{"markdown": "p1"}]

    def test_capabilities_unwrapped(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api")
        caps = {"version": "0.14.0", "formats": {"supported": ["markdown"]}}
        with patch.object(client, "_http_request", return_value=caps) as mock_req:
            result = client.capabilities()
        assert mock_req.call_args[0][:2] == ("GET", "/v1/capabilities")
        assert result == caps

    def test_change_tracking_diff_body(self) -> None:
        client = CrwClient(api_url="https://fastcrw.com/api")
        with patch.object(client, "_http_post", return_value={"diff": "..."}) as mock_post:
            client.change_tracking_diff(
                current={"markdown": "new"}, previous={"markdown": "old"}, modes=["gitDiff"]
            )
        assert mock_post.call_args[0][0] == "/v1/change-tracking/diff"
        body = mock_post.call_args[0][1]
        assert body["current"] == {"markdown": "new"}
        assert body["previous"] == {"markdown": "old"}
        assert body["modes"] == ["gitDiff"]
