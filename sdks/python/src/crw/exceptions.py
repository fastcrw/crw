"""CRW SDK exceptions."""

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from crw.types import ExtractStatus, ExtractUrlResult


class CrwError(Exception):
    """Base exception for CRW SDK."""


class CrwBinaryNotFoundError(CrwError):
    """Binary could not be found or downloaded."""


class CrwTimeoutError(CrwError):
    """Operation timed out."""


class CrwExtractCancelledError(CrwError):
    """An extract waiter reached the immutable cancelled state."""

    def __init__(self, status: "ExtractStatus"):
        super().__init__(f"Extract {status['id']} was cancelled")
        self.status = status
        self.results: list["ExtractUrlResult"] = status["results"]


class CrwApiError(CrwError):
    """API returned an error."""

    def __init__(
        self,
        message: str,
        status_code: int | None = None,
        error_code: str | None = None,
    ):
        super().__init__(message)
        self.status_code = status_code
        # The body's machine-readable ``errorCode``, when the server sent one.
        self.error_code = error_code
