/** CRW SDK error types. */

import type { ExtractStatus } from "./types.js";

export class CrwError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "CrwError";
  }
}

export class CrwApiError extends CrwError {
  statusCode?: number;
  /** The body's machine-readable `errorCode`, when the server sent one. */
  errorCode?: string;
  constructor(message: string, statusCode?: number, errorCode?: string) {
    super(message);
    this.name = "CrwApiError";
    this.statusCode = statusCode;
    this.errorCode = errorCode;
  }
}

export class CrwTimeoutError extends CrwError {
  constructor(message: string) {
    super(message);
    this.name = "CrwTimeoutError";
  }
}

/** A convenience extract waiter reached the immutable cancelled state. */
export class CrwExtractCancelledError extends CrwError {
  readonly status: ExtractStatus;
  readonly results: ExtractStatus["results"];

  constructor(status: ExtractStatus) {
    super(`Extract ${status.id} was cancelled`);
    this.name = "CrwExtractCancelledError";
    this.status = status;
    this.results = status.results;
  }
}

export class CrwBinaryNotFoundError extends CrwError {
  constructor(message: string) {
    super(message);
    this.name = "CrwBinaryNotFoundError";
  }
}
