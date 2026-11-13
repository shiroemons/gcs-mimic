# gRPC deadlines and client cancellation

This record covers the deadline and cancellation case of BEHAVIOR-RPC-005.

gRPC clients enforce a deadline or a cancellation by resetting the HTTP/2
stream. The emulator serves Storage v2 through its own hyper connection loop
and does not apply a server-side `grpc-timeout` limit, so the observable
contract is what an abandoned call leaves behind.

## Test

`TestGRPCDeadlineAndCancellation` (`sdk-tests/go/grpc_deadline_test.go`) uses
one `grpc-go` connection, pinned with the rest of the Go suite, whose stream
and connection windows are fixed at 64 KiB so that a reader that stops reading
leaves the server blocked on flow control. On that connection it checks four
call shapes:

| Call | How it ends | Checked afterwards |
| --- | --- | --- |
| `WriteObject` (client stream) | 300 ms deadline after one unfinished chunk | `DEADLINE_EXCEEDED`; no object is published; a new `WriteObject` with `if_generation_match: 0` for the same name succeeds |
| `BidiWriteObject` (resumable) | cancellation after a `state_lookup` acknowledged 3 bytes | `CANCELED`; no object is published; `QueryWriteStatus` reports 3; resuming at offset 3 with `finish_write` completes the object as `abcdef` |
| `ReadObject` (server stream, 8 MiB object) | 300 ms deadline after the first message | `DEADLINE_EXCEEDED` |
| `BidiReadObject` (8 MiB object) | cancellation after the first message | `CANCELED` |

After each call, a unary `GetObject` on the same connection succeeds.

## Results

`GCSM_GO_TEST_RUN=TestGRPCDeadlineAndCancellation bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh`
ran three times on 2026-11-13 on macOS ARM64. Each run passed, in 0.75 s,
0.73 s and 0.76 s.

## Finding: unacknowledged bytes can commit after a status query

The first version of the test also sent 3 more bytes without a `state_lookup`
and cancelled right after sending them. `QueryWriteStatus` then reported 3,
and resuming at offset 3 failed with `OUT_OF_RANGE` ("write_offset does not
match persisted data"): the server had already received the unacknowledged
message and committed it after the query answered.

`BidiWriteObject` runs in a supervised task, takes the session lease for each
message, and finishes a message it has received even after the client resets
the stream. `QueryWriteStatus` reads the session without waiting for that
lease. A client that resumes from a status queried right after a cancellation
must therefore expect `OUT_OF_RANGE` and query again. This is recorded on
RPC-STORAGE-019 together with the pending case for concurrent streams on one
`upload_id`; the test now cancels only after an acknowledgement.
