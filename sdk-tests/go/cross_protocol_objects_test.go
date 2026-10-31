package gcsmimic_test

import (
	"bytes"
	"context"
	"crypto/md5"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"encoding/xml"
	"errors"
	"fmt"
	"hash/crc32"
	"io"
	"maps"
	"net"
	"net/http"
	"net/url"
	"os"
	"reflect"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/iterator"
	pb "google.golang.org/genproto/googleapis/storage/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/types/known/wrapperspb"
)

// These tests are the cross-protocol integration gate: each object is written through one
// transport and observed or mutated through the others against one emulator. JSON and XML use
// raw HTTP so the wire representations (string generations, x-goog-* headers, XML error codes)
// are asserted directly; Storage v2 metadata uses the SDK's pinned descriptors over the wire and
// v2 bytes, writes, updates and listing use the ordinary SDK gRPC client. Legacy v1 has no media
// RPC in its supported subset, so it observes metadata and listings only.

const crossContentType = "text/plain"

// crossView is the transport-neutral projection of one object generation.
type crossView struct {
	Generation, Metageneration, Size int64
	CRC32C                           uint32
	MD5                              []byte
	ContentType                      string
	Metadata                         map[string]string
	Body                             []byte
}

func expectedView(generation int64, origin string, payload []byte) crossView {
	sum := md5.Sum(payload)
	return crossView{
		Generation:     generation,
		Metageneration: 1,
		Size:           int64(len(payload)),
		CRC32C:         crc32.Checksum(payload, crc32.MakeTable(crc32.Castagnoli)),
		MD5:            sum[:],
		ContentType:    crossContentType,
		Metadata:       map[string]string{"origin": origin},
		Body:           payload,
	}
}

type httpStatusError struct {
	status int
	body   string
}

func (e *httpStatusError) Error() string { return fmt.Sprintf("HTTP %d: %s", e.status, e.body) }

type crossEnv struct {
	t        *testing.T
	ctx      context.Context
	http     *http.Client
	endpoint *url.URL
	bucket   string
	conn     *grpc.ClientConn
	v1       pb.StorageClient
	sdk      *storage.Client
	token    string
}

// newCrossEnv connects to the HTTP endpoint and the h2c listener of one emulator. The SDK gRPC
// client reads STORAGE_EMULATOR_HOST_GRPC, so the caller selects the listener before this call.
func newCrossEnv(t *testing.T, ctx context.Context, httpClient *http.Client, endpoint *url.URL, grpcEndpoint, bucket string) *crossEnv {
	t.Helper()
	host, _, err := net.SplitHostPort(grpcEndpoint)
	if err != nil || net.ParseIP(host) == nil || !net.ParseIP(host).IsLoopback() {
		t.Fatal("cross-protocol tests require a loopback gRPC endpoint")
	}
	t.Setenv("STORAGE_EMULATOR_HOST_GRPC", grpcEndpoint)
	conn, err := grpc.NewClient(grpcEndpoint, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { conn.Close() })
	sdk, err := storage.NewGRPCClient(ctx)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { sdk.Close() })
	return &crossEnv{t: t, ctx: ctx, http: httpClient, endpoint: endpoint, bucket: bucket, conn: conn, v1: pb.NewStorageClient(conn), sdk: sdk}
}

// as returns the same environment acting as another local principal; "" is anonymous.
func (e *crossEnv) as(token string) *crossEnv {
	copied := *e
	copied.token = token
	return &copied
}

func (e *crossEnv) grpcContext() context.Context {
	if e.token == "" {
		return e.ctx
	}
	return metadata.AppendToOutgoingContext(e.ctx, "authorization", "Bearer "+e.token)
}

func (e *crossEnv) send(method, path string, query url.Values, header http.Header, body []byte) (*http.Response, []byte, error) {
	e.t.Helper()
	target := *e.endpoint
	target.Path = strings.TrimSuffix(e.endpoint.Path, "/") + path
	target.RawQuery = query.Encode()
	var reader io.Reader
	if body != nil {
		reader = bytes.NewReader(body)
	}
	request, err := http.NewRequestWithContext(e.ctx, method, target.String(), reader)
	if err != nil {
		e.t.Fatal(err)
	}
	for key, values := range header {
		for _, value := range values {
			request.Header.Add(key, value)
		}
	}
	if e.token != "" {
		request.Header.Set("Authorization", "Bearer "+e.token)
	}
	response, err := e.http.Do(request)
	if err != nil {
		e.t.Fatal(err)
	}
	data, err := io.ReadAll(response.Body)
	response.Body.Close()
	if err != nil {
		e.t.Fatal(err)
	}
	if response.StatusCode >= 300 {
		return response, data, &httpStatusError{response.StatusCode, string(data)}
	}
	return response, data, nil
}

func (e *crossEnv) jsonPath(name string) string {
	return "/storage/v1/b/" + e.bucket + "/o/" + url.PathEscape(name)
}

func (e *crossEnv) xmlPath(name string) string { return "/" + e.bucket + "/" + url.PathEscape(name) }

func generationQuery(generation int64, extra ...string) url.Values {
	query := url.Values{}
	if generation > 0 {
		query.Set("generation", strconv.FormatInt(generation, 10))
	}
	for i := 0; i+1 < len(extra); i += 2 {
		query.Set(extra[i], extra[i+1])
	}
	return query
}

// writeJSON uses a multipart upload so metadata and media are published in one JSON request.
func (e *crossEnv) writeJSON(name string, payload []byte, ifGenerationMatch *int64) (int64, error) {
	e.t.Helper()
	resource, _ := json.Marshal(map[string]any{"name": name, "contentType": crossContentType, "metadata": map[string]string{"origin": "json"}})
	var body bytes.Buffer
	fmt.Fprintf(&body, "--cross\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n%s\r\n--cross\r\nContent-Type: %s\r\n\r\n", resource, crossContentType)
	body.Write(payload)
	body.WriteString("\r\n--cross--\r\n")
	query := url.Values{"uploadType": {"multipart"}}
	if ifGenerationMatch != nil {
		query.Set("ifGenerationMatch", strconv.FormatInt(*ifGenerationMatch, 10))
	}
	_, data, err := e.send(http.MethodPost, "/upload/storage/v1/b/"+e.bucket+"/o", query, http.Header{"Content-Type": {"multipart/related; boundary=cross"}}, body.Bytes())
	if err != nil {
		return 0, err
	}
	var object struct {
		Generation string `json:"generation"`
	}
	if err := json.Unmarshal(data, &object); err != nil {
		e.t.Fatal(err)
	}
	return strconv.ParseInt(object.Generation, 10, 64)
}

func (e *crossEnv) writeXML(name string, payload []byte, ifGenerationMatch *int64) (int64, error) {
	e.t.Helper()
	header := http.Header{"Content-Type": {crossContentType}, "X-Goog-Meta-Origin": {"xml"}}
	if ifGenerationMatch != nil {
		header.Set("X-Goog-If-Generation-Match", strconv.FormatInt(*ifGenerationMatch, 10))
	}
	response, _, err := e.send(http.MethodPut, e.xmlPath(name), nil, header, payload)
	if err != nil {
		return 0, err
	}
	return strconv.ParseInt(response.Header.Get("X-Goog-Generation"), 10, 64)
}

func (e *crossEnv) writeGRPC(name string, payload []byte, ifGenerationMatch *int64) (int64, error) {
	object := e.sdk.Bucket(e.bucket).Object(name)
	if ifGenerationMatch != nil {
		object = object.If(storage.Conditions{GenerationMatch: *ifGenerationMatch})
	}
	writer := object.NewWriter(e.grpcContext())
	writer.ContentType = crossContentType
	writer.Metadata = map[string]string{"origin": "grpc"}
	if _, err := writer.Write(payload); err != nil {
		writer.Close()
		return 0, err
	}
	if err := writer.Close(); err != nil {
		return 0, err
	}
	return writer.Attrs().Generation, nil
}

func (e *crossEnv) write(transport, name string, payload []byte, ifGenerationMatch *int64) (int64, error) {
	switch transport {
	case "json":
		return e.writeJSON(name, payload, ifGenerationMatch)
	case "xml":
		return e.writeXML(name, payload, ifGenerationMatch)
	case "grpc":
		return e.writeGRPC(name, payload, ifGenerationMatch)
	}
	e.t.Fatalf("unknown transport %s", transport)
	return 0, nil
}

func decodeBase64(t *testing.T, label, value string) []byte {
	t.Helper()
	decoded, err := base64.StdEncoding.DecodeString(value)
	if err != nil {
		t.Fatalf("%s base64 %q: %v", label, value, err)
	}
	return decoded
}

func (e *crossEnv) viewJSON(name string, generation int64) (crossView, error) {
	e.t.Helper()
	_, data, err := e.send(http.MethodGet, e.jsonPath(name), generationQuery(generation), nil, nil)
	if err != nil {
		return crossView{}, err
	}
	var resource map[string]any
	if err := json.Unmarshal(data, &resource); err != nil {
		e.t.Fatal(err)
	}
	view := crossView{Metadata: map[string]string{}}
	for field, target := range map[string]*int64{"generation": &view.Generation, "metageneration": &view.Metageneration, "size": &view.Size} {
		// JSON API int64 fields are decimal strings; a JSON number would be a representation bug.
		raw, ok := resource[field].(string)
		if !ok {
			e.t.Fatalf("JSON %s is %T, want a decimal string", field, resource[field])
		}
		if *target, err = strconv.ParseInt(raw, 10, 64); err != nil {
			e.t.Fatal(err)
		}
	}
	crc := decodeBase64(e.t, "JSON crc32c", resource["crc32c"].(string))
	if len(crc) != 4 {
		e.t.Fatalf("JSON crc32c has %d bytes", len(crc))
	}
	view.CRC32C = binary.BigEndian.Uint32(crc)
	view.MD5 = decodeBase64(e.t, "JSON md5Hash", resource["md5Hash"].(string))
	view.ContentType, _ = resource["contentType"].(string)
	if values, ok := resource["metadata"].(map[string]any); ok {
		for key, value := range values {
			view.Metadata[key] = value.(string)
		}
	}
	_, view.Body, err = e.send(http.MethodGet, e.jsonPath(name), generationQuery(generation, "alt", "media"), nil, nil)
	return view, err
}

func (e *crossEnv) viewXML(name string, generation int64) (crossView, error) {
	e.t.Helper()
	response, body, err := e.send(http.MethodGet, e.xmlPath(name), generationQuery(generation), nil, nil)
	if err != nil {
		return crossView{}, err
	}
	view := crossView{ContentType: response.Header.Get("Content-Type"), Metadata: map[string]string{}, Body: body}
	for header, target := range map[string]*int64{"X-Goog-Generation": &view.Generation, "X-Goog-Metageneration": &view.Metageneration, "X-Goog-Stored-Content-Length": &view.Size} {
		if *target, err = strconv.ParseInt(response.Header.Get(header), 10, 64); err != nil {
			e.t.Fatalf("XML %s: %v", header, err)
		}
	}
	for _, value := range response.Header.Values("X-Goog-Hash") {
		for _, item := range strings.Split(value, ",") {
			kind, digest, _ := strings.Cut(strings.TrimSpace(item), "=")
			switch kind {
			case "crc32c":
				view.CRC32C = binary.BigEndian.Uint32(decodeBase64(e.t, "XML crc32c", digest))
			case "md5":
				view.MD5 = decodeBase64(e.t, "XML md5", digest)
			}
		}
	}
	for key, values := range response.Header {
		if suffix, ok := strings.CutPrefix(key, "X-Goog-Meta-"); ok {
			view.Metadata[strings.ToLower(suffix)] = values[0]
		}
	}
	return view, nil
}

func protoField(message protoreflect.Message, name string) protoreflect.Value {
	return message.Get(message.Descriptor().Fields().ByName(protoreflect.Name(name)))
}

// getV2 calls Storage v2 GetObject on the wire so int64 fields and status codes are not
// reinterpreted by the SDK.
func (e *crossEnv) getV2(name string, fields map[string]any) (protoreflect.Message, error) {
	request := map[string]any{"bucket": "projects/_/buckets/" + e.bucket, "object": name}
	for key, value := range fields {
		request[key] = value
	}
	response := grpcIOMessage(e.t, "Object", nil)
	err := e.conn.Invoke(e.grpcContext(), "/google.storage.v2.Storage/GetObject", grpcIOMessage(e.t, "GetObjectRequest", request), response)
	return response, err
}

func (e *crossEnv) viewGRPC(name string, generation int64) (crossView, error) {
	e.t.Helper()
	fields := map[string]any{}
	if generation > 0 {
		fields["generation"] = generation
	}
	object, err := e.getV2(name, fields)
	if err != nil {
		return crossView{}, err
	}
	checksums := protoField(object, "checksums").Message()
	view := crossView{
		Generation:     protoField(object, "generation").Int(),
		Metageneration: protoField(object, "metageneration").Int(),
		Size:           protoField(object, "size").Int(),
		CRC32C:         uint32(protoField(checksums, "crc32c").Uint()),
		MD5:            protoField(checksums, "md5_hash").Bytes(),
		ContentType:    protoField(object, "content_type").String(),
		Metadata:       map[string]string{},
	}
	protoField(object, "metadata").Map().Range(func(key protoreflect.MapKey, value protoreflect.Value) bool {
		view.Metadata[key.String()] = value.String()
		return true
	})
	handle := e.sdk.Bucket(e.bucket).Object(name)
	if generation > 0 {
		handle = handle.Generation(generation)
	}
	reader, err := handle.NewReader(e.grpcContext())
	if err != nil {
		return view, err
	}
	defer reader.Close()
	view.Body, err = io.ReadAll(reader)
	return view, err
}

func (e *crossEnv) viewV1(name string, generation int64) (crossView, error) {
	object, err := e.v1.GetObject(e.grpcContext(), &pb.GetObjectRequest{Bucket: e.bucket, Object: name, Generation: generation})
	if err != nil {
		return crossView{}, err
	}
	view := crossView{
		Generation:     object.GetGeneration(),
		Metageneration: object.GetMetageneration(),
		Size:           object.GetSize(),
		CRC32C:         object.GetCrc32C().GetValue(),
		MD5:            decodeBase64(e.t, "v1 md5Hash", object.GetMd5Hash()),
		ContentType:    object.GetContentType(),
		Metadata:       map[string]string{},
	}
	for key, value := range object.GetMetadata() {
		view.Metadata[key] = value
	}
	return view, nil
}

type crossObserver struct {
	name string
	view func(*crossEnv, string, int64) (crossView, error)
	// The legacy v1 subset has no media RPC, so its view never carries bytes.
	metadataOnly bool
}

var crossObservers = []crossObserver{
	{"json", (*crossEnv).viewJSON, false},
	{"xml", (*crossEnv).viewXML, false},
	{"grpc", (*crossEnv).viewGRPC, false},
	{"grpc-v1", (*crossEnv).viewV1, true},
}

// requireViews asserts that every transport observes the same generation state.
func (e *crossEnv) requireViews(label, name string, generation int64, want crossView) {
	e.t.Helper()
	for _, observer := range crossObservers {
		got, err := observer.view(e, name, generation)
		if err != nil {
			e.t.Fatalf("%s: %s view of %s: %v", label, observer.name, name, err)
		}
		expected := want
		if observer.metadataOnly {
			expected.Body = nil
		}
		if !reflect.DeepEqual(got, expected) {
			e.t.Fatalf("%s: %s view of %s\n got %+v\nwant %+v", label, observer.name, name, got, expected)
		}
	}
}

func requireHTTPError(t *testing.T, label string, err error, code int, xmlCode string) {
	t.Helper()
	var httpError *httpStatusError
	if !errors.As(err, &httpError) || httpError.status != code {
		t.Fatalf("%s: want HTTP %d, got %v", label, code, err)
	}
	if xmlCode != "" && !strings.Contains(httpError.body, "<Code>"+xmlCode+"</Code>") {
		t.Fatalf("%s: want XML code %s, got %s", label, xmlCode, httpError.body)
	}
}

func requireCode(t *testing.T, label string, err error, code codes.Code) {
	t.Helper()
	if status.Code(err) != code {
		t.Fatalf("%s: want %v, got %v", label, code, err)
	}
}

// checkCode reports a mismatch without stopping, so one run lists every transport whose
// authorization decision diverges.
func checkCode(t *testing.T, label string, err error, code codes.Code) {
	t.Helper()
	if status.Code(err) != code {
		t.Errorf("%s: want %v, got %v", label, code, err)
	}
}

func checkHTTPError(t *testing.T, label string, err error, code int, xmlCode string) {
	t.Helper()
	var httpError *httpStatusError
	if !errors.As(err, &httpError) || httpError.status != code {
		t.Errorf("%s: want HTTP %d, got %v", label, code, err)
	} else if xmlCode != "" && !strings.Contains(httpError.body, "<Code>"+xmlCode+"</Code>") {
		t.Errorf("%s: want XML code %s, got %s", label, xmlCode, httpError.body)
	}
}

// requireAbsent asserts protocol-specific not-found results for one name or generation.
func (e *crossEnv) requireAbsent(label, name string, generation int64) {
	e.t.Helper()
	_, _, err := e.send(http.MethodGet, e.jsonPath(name), generationQuery(generation), nil, nil)
	requireHTTPError(e.t, label+" JSON", err, http.StatusNotFound, "")
	_, _, err = e.send(http.MethodGet, e.xmlPath(name), generationQuery(generation), nil, nil)
	requireHTTPError(e.t, label+" XML", err, http.StatusNotFound, "NoSuchKey")
	fields := map[string]any{}
	if generation > 0 {
		fields["generation"] = generation
	}
	_, err = e.getV2(name, fields)
	requireCode(e.t, label+" gRPC", err, codes.NotFound)
	_, err = e.v1.GetObject(e.grpcContext(), &pb.GetObjectRequest{Bucket: e.bucket, Object: name, Generation: generation})
	requireCode(e.t, label+" gRPC v1", err, codes.NotFound)
}

func (e *crossEnv) listJSON(versions bool) map[string][]int64 {
	e.t.Helper()
	query := url.Values{}
	if versions {
		query.Set("versions", "true")
	}
	_, data, err := e.send(http.MethodGet, "/storage/v1/b/"+e.bucket+"/o", query, nil, nil)
	if err != nil {
		e.t.Fatal(err)
	}
	var page struct {
		Items []struct {
			Name       string `json:"name"`
			Generation string `json:"generation"`
		} `json:"items"`
		NextPageToken string `json:"nextPageToken"`
	}
	if err := json.Unmarshal(data, &page); err != nil {
		e.t.Fatal(err)
	}
	if page.NextPageToken != "" {
		e.t.Fatal("cross-protocol listings must fit one JSON page")
	}
	listed := map[string][]int64{}
	for _, item := range page.Items {
		generation, err := strconv.ParseInt(item.Generation, 10, 64)
		if err != nil {
			e.t.Fatal(err)
		}
		listed[item.Name] = append(listed[item.Name], generation)
	}
	return listed
}

func (e *crossEnv) listGRPC(versions bool) (map[string][]int64, error) {
	listed := map[string][]int64{}
	objects := e.sdk.Bucket(e.bucket).Objects(e.grpcContext(), &storage.Query{Versions: versions})
	for {
		attrs, err := objects.Next()
		if errors.Is(err, iterator.Done) {
			return listed, nil
		}
		if err != nil {
			return nil, err
		}
		listed[attrs.Name] = append(listed[attrs.Name], attrs.Generation)
	}
}

func (e *crossEnv) listV1(versions bool) (map[string][]int64, error) {
	response, err := e.v1.ListObjects(e.grpcContext(), &pb.ListObjectsRequest{Bucket: e.bucket, Versions: versions})
	if err != nil {
		return nil, err
	}
	if response.GetNextPageToken() != "" {
		e.t.Fatal("cross-protocol listings must fit one v1 page")
	}
	listed := map[string][]int64{}
	for _, item := range response.GetItems() {
		listed[item.GetName()] = append(listed[item.GetName()], item.GetGeneration())
	}
	return listed, nil
}

// requireListings asserts that JSON, Storage v2 and legacy v1 list the same names and
// generations. XML's selected entry metadata is checked separately.
func (e *crossEnv) requireListings(label string, versions bool, want map[string][]int64) {
	e.t.Helper()
	normalize := func(listing map[string][]int64) map[string][]int64 {
		for _, generations := range listing {
			slices.Sort(generations)
		}
		return listing
	}
	want = normalize(want)
	if got := normalize(e.listJSON(versions)); !reflect.DeepEqual(got, want) {
		e.t.Fatalf("%s: JSON listing %v, want %v", label, got, want)
	}
	got, err := e.listGRPC(versions)
	if err != nil || !reflect.DeepEqual(normalize(got), want) {
		e.t.Fatalf("%s: gRPC listing %v error=%v, want %v", label, got, err, want)
	}
	got, err = e.listV1(versions)
	if err != nil || !reflect.DeepEqual(normalize(got), want) {
		e.t.Fatalf("%s: gRPC v1 listing %v error=%v, want %v", label, got, err, want)
	}
}

func (e *crossEnv) requireXMLListing(label string, want map[string]crossView) {
	e.t.Helper()
	_, data, err := e.send(http.MethodGet, "/"+e.bucket, nil, nil, nil)
	if err != nil {
		e.t.Fatal(err)
	}
	var page struct {
		Name        string
		IsTruncated bool
		Contents    []struct {
			Key                              string
			Generation, MetaGeneration, Size int64
		}
	}
	if err := xml.Unmarshal(data, &page); err != nil {
		e.t.Fatal(err)
	}
	if page.Name != e.bucket || page.IsTruncated || len(page.Contents) != len(want) {
		e.t.Fatalf("%s: XML listing %+v, want %d entries from %s", label, page, len(want), e.bucket)
	}
	seen := map[string]bool{}
	for _, item := range page.Contents {
		view, exists := want[item.Key]
		if !exists || seen[item.Key] || item.Generation != view.Generation || item.MetaGeneration != view.Metageneration || item.Size != view.Size {
			e.t.Fatalf("%s: XML entry %+v, want %+v; duplicate=%v", label, item, view, seen[item.Key])
		}
		seen[item.Key] = true
	}
}

func crossGRPCEndpoint(t *testing.T, variable string) string {
	t.Helper()
	endpoint := os.Getenv(variable)
	if endpoint == "" {
		t.Skipf("%s is unset", variable)
	}
	return endpoint
}

func TestCrossProtocolObjects(t *testing.T) {
	grpcEndpoint := crossGRPCEndpoint(t, "STORAGE_EMULATOR_HOST_GRPC")
	ctx, jsonClient, httpClient, endpoint := newEmulatorClient(t)
	bucket := resumableBucket(t, ctx, jsonClient, "cross-protocol")
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	env := newCrossEnv(t, ctx, httpClient, endpoint, grpcEndpoint, bucket.BucketName())
	transports := []string{"json", "xml", "grpc"}
	t.Cleanup(func() {
		cleanup, stop := context.WithTimeout(context.Background(), 5*time.Second)
		defer stop()
		for _, transport := range transports {
			_ = bucket.Object("from-" + transport).Delete(cleanup)
		}
	})

	// Write through each transport and observe the same generation through every other one.
	views := map[string]crossView{}
	for _, transport := range transports {
		name := "from-" + transport
		payload := []byte("written through " + transport)
		generation, err := env.write(transport, name, payload, nil)
		if err != nil || generation <= 0 {
			t.Fatalf("%s write: generation=%d error=%v", transport, generation, err)
		}
		views[name] = expectedView(generation, transport, payload)
		env.requireViews(transport+" write", name, 0, views[name])
		env.requireViews(transport+" write selected generation", name, generation, views[name])
	}

	listing := func() map[string][]int64 {
		listed := map[string][]int64{}
		for name, view := range views {
			listed[name] = []int64{view.Generation}
		}
		return listed
	}
	env.requireListings("after writes", false, listing())
	env.requireXMLListing("after writes", views)
	_, _, err := env.send(http.MethodGet, "/"+env.bucket, url.Values{"list-type": {"2"}, "fetch-owner": {"true"}}, nil, nil)
	requireHTTPError(t, "unsupported XML owner listing", err, http.StatusBadRequest, "InvalidArgument")
	for name, view := range views {
		env.requireViews("unsupported XML owner listing preserves state", name, 0, view)
	}

	// A JSON PATCH bumps the metageneration observed by XML and both gRPC services.
	patched := views["from-xml"]
	_, _, err = env.send(http.MethodPatch, env.jsonPath("from-xml"), url.Values{"ifMetagenerationMatch": {"1"}},
		http.Header{"Content-Type": {"application/json"}}, []byte(`{"metadata":{"patched":"json"}}`))
	if err != nil {
		t.Fatal(err)
	}
	patched.Metageneration = 2
	patched.Metadata = map[string]string{"origin": "xml", "patched": "json"}
	views["from-xml"] = patched
	env.requireViews("JSON PATCH", "from-xml", 0, patched)
	_, err = env.sdk.Bucket(env.bucket).Object("from-xml").If(storage.Conditions{MetagenerationMatch: 1}).
		Update(ctx, storage.ObjectAttrsToUpdate{Metadata: map[string]string{"stale": "grpc"}})
	requireCode(t, "gRPC update with metageneration observed before JSON PATCH", err, codes.FailedPrecondition)

	// A gRPC UpdateObject bumps the metageneration observed by JSON, XML and v1.
	updated := views["from-json"]
	if _, err := env.sdk.Bucket(env.bucket).Object("from-json").If(storage.Conditions{MetagenerationMatch: 1}).
		Update(ctx, storage.ObjectAttrsToUpdate{Metadata: map[string]string{"updated": "grpc"}}); err != nil {
		t.Fatal(err)
	}
	updated.Metageneration = 2
	updated.Metadata = map[string]string{"origin": "json", "updated": "grpc"}
	views["from-json"] = updated
	env.requireViews("gRPC UpdateObject", "from-json", 0, updated)
	_, _, err = env.send(http.MethodPatch, env.jsonPath("from-json"), url.Values{"ifMetagenerationMatch": {"1"}},
		http.Header{"Content-Type": {"application/json"}}, []byte(`{"metadata":{"stale":"json"}}`))
	requireHTTPError(t, "JSON PATCH with metageneration observed before gRPC update", err, http.StatusPreconditionFailed, "")
	env.requireViews("rejected stale mutations", "from-json", 0, updated)
	env.requireViews("rejected stale mutations", "from-xml", 0, patched)

	// Read preconditions built from one transport's view hold on every other transport.
	xmlView, err := env.viewXML("from-grpc", 0)
	if err != nil {
		t.Fatal(err)
	}
	current, stale := xmlView.Generation, xmlView.Generation+1
	match := func(generation int64) string { return strconv.FormatInt(generation, 10) }
	if _, _, err := env.send(http.MethodGet, env.jsonPath("from-grpc"), url.Values{"ifGenerationMatch": {match(current)}}, nil, nil); err != nil {
		t.Fatalf("JSON read with XML generation: %v", err)
	}
	if _, err := env.getV2("from-grpc", map[string]any{"if_generation_match": current}); err != nil {
		t.Fatalf("gRPC read with XML generation: %v", err)
	}
	if _, err := env.v1.GetObject(ctx, &pb.GetObjectRequest{Bucket: env.bucket, Object: "from-grpc", IfGenerationMatch: wrapperspb.Int64(current)}); err != nil {
		t.Fatalf("gRPC v1 read with XML generation: %v", err)
	}
	_, _, err = env.send(http.MethodGet, env.jsonPath("from-grpc"), url.Values{"ifGenerationMatch": {match(stale)}}, nil, nil)
	requireHTTPError(t, "JSON stale read precondition", err, http.StatusPreconditionFailed, "")
	_, _, err = env.send(http.MethodGet, env.xmlPath("from-grpc"), nil, http.Header{"X-Goog-If-Generation-Match": {match(stale)}}, nil)
	requireHTTPError(t, "XML stale read precondition", err, http.StatusPreconditionFailed, "PreconditionFailed")
	_, err = env.getV2("from-grpc", map[string]any{"if_generation_match": stale})
	requireCode(t, "gRPC stale read precondition", err, codes.FailedPrecondition)
	_, err = env.v1.GetObject(ctx, &pb.GetObjectRequest{Bucket: env.bucket, Object: "from-grpc", IfGenerationMatch: wrapperspb.Int64(stale)})
	requireCode(t, "gRPC v1 stale read precondition", err, codes.FailedPrecondition)

	// An XML overwrite conditioned on the gRPC-written generation replaces it for everyone, and
	// writes conditioned on the replaced generation fail on every transport.
	grpcView, err := env.viewGRPC("from-grpc", 0)
	if err != nil {
		t.Fatal(err)
	}
	replaced := grpcView.Generation
	payload := []byte("replaced through xml")
	generation, err := env.writeXML("from-grpc", payload, &replaced)
	if err != nil || generation == replaced {
		t.Fatalf("conditional XML overwrite: generation=%d error=%v", generation, err)
	}
	views["from-grpc"] = expectedView(generation, "xml", payload)
	env.requireViews("conditional XML overwrite", "from-grpc", 0, views["from-grpc"])
	_, err = env.writeXML("from-grpc", []byte("stale"), &replaced)
	requireHTTPError(t, "XML stale overwrite", err, http.StatusPreconditionFailed, "PreconditionFailed")
	_, err = env.writeJSON("from-grpc", []byte("stale"), &replaced)
	requireHTTPError(t, "JSON stale overwrite", err, http.StatusPreconditionFailed, "")
	_, err = env.writeGRPC("from-grpc", []byte("stale"), &replaced)
	requireCode(t, "gRPC stale overwrite", err, codes.FailedPrecondition)
	env.requireViews("rejected stale overwrites", "from-grpc", 0, views["from-grpc"])
	env.requireListings("after mutations", false, listing())
	env.requireXMLListing("after mutations", views)

	// Delete through each transport with a generation observed elsewhere; a stale generation
	// fails first and leaves the object visible.
	for _, test := range []struct {
		object, deleter string
		remove          func(name string, generation int64) error
	}{
		{"from-json", "grpc", func(name string, generation int64) error {
			return env.sdk.Bucket(env.bucket).Object(name).If(storage.Conditions{GenerationMatch: generation}).Delete(ctx)
		}},
		{"from-xml", "json", func(name string, generation int64) error {
			_, _, err := env.send(http.MethodDelete, env.jsonPath(name), url.Values{"ifGenerationMatch": {match(generation)}}, nil, nil)
			return err
		}},
		{"from-grpc", "xml", func(name string, generation int64) error {
			_, _, err := env.send(http.MethodDelete, env.xmlPath(name), nil, http.Header{"X-Goog-If-Generation-Match": {match(generation)}}, nil)
			return err
		}},
	} {
		label := test.deleter + " delete of " + test.object
		err := test.remove(test.object, views[test.object].Generation+1)
		switch test.deleter {
		case "grpc":
			requireCode(t, label+" with stale generation", err, codes.FailedPrecondition)
		case "json":
			requireHTTPError(t, label+" with stale generation", err, http.StatusPreconditionFailed, "")
		case "xml":
			requireHTTPError(t, label+" with stale generation", err, http.StatusPreconditionFailed, "PreconditionFailed")
		}
		env.requireViews(label+" rejected", test.object, 0, views[test.object])
		if err := test.remove(test.object, views[test.object].Generation); err != nil {
			t.Fatalf("%s: %v", label, err)
		}
		env.requireAbsent(label, test.object, 0)
		env.requireAbsent(label+" selected generation", test.object, views[test.object].Generation)
		delete(views, test.object)
		env.requireListings("after "+label, false, listing())
		env.requireXMLListing("after "+label, views)
	}
}

func TestCrossProtocolObjectVersions(t *testing.T) {
	grpcEndpoint := crossGRPCEndpoint(t, "STORAGE_EMULATOR_HOST_GRPC")
	ctx, bucket := versioningBucket(t, &storage.BucketAttrs{VersioningEnabled: true})
	_, _, httpClient, endpoint := newEmulatorClient(t)
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	env := newCrossEnv(t, ctx, httpClient, endpoint, grpcEndpoint, bucket.BucketName())

	first, err := env.writeXML("doc", []byte("first via xml"), nil)
	if err != nil {
		t.Fatal(err)
	}
	second, err := env.writeGRPC("doc", []byte("second via grpc"), &first)
	if err != nil {
		t.Fatal(err)
	}
	versions := map[int64]crossView{
		first:  expectedView(first, "xml", []byte("first via xml")),
		second: expectedView(second, "grpc", []byte("second via grpc")),
	}
	env.requireViews("live after gRPC overwrite", "doc", 0, versions[second])
	env.requireViews("noncurrent after gRPC overwrite", "doc", first, versions[first])

	// Deleting the live object through JSON leaves both generations addressable everywhere.
	if _, _, err := env.send(http.MethodDelete, env.jsonPath("doc"), url.Values{"ifGenerationMatch": {strconv.FormatInt(second, 10)}}, nil, nil); err != nil {
		t.Fatal(err)
	}
	env.requireAbsent("live after JSON delete", "doc", 0)
	for generation, view := range versions {
		env.requireViews(fmt.Sprintf("noncurrent generation %d", generation), "doc", generation, view)
	}
	env.requireListings("live listing after delete", false, map[string][]int64{})
	env.requireListings("version listing after delete", true, map[string][]int64{"doc": {first, second}})

	// Removing one noncurrent generation through XML removes only that generation.
	if _, _, err := env.send(http.MethodDelete, env.xmlPath("doc"), generationQuery(first), nil, nil); err != nil {
		t.Fatal(err)
	}
	env.requireAbsent("XML-deleted generation", "doc", first)
	env.requireViews("remaining noncurrent generation", "doc", second, versions[second])
	env.requireListings("version listing after XML generation delete", true, map[string][]int64{"doc": {second}})
}

func strictCrossHTTP(t *testing.T) (*http.Client, *url.URL) {
	t.Helper()
	u, err := emulatorEndpoint(os.Getenv("GCS_MIMIC_STRICT_ENDPOINT"), os.Getenv("GCS_MIMIC_ALLOWED_HOSTS"))
	if err != nil {
		t.Fatal(err)
	}
	transport := &http.Transport{TLSClientConfig: trustedEmulatorTLS(t), Proxy: nil, MaxConnsPerHost: 2, IdleConnTimeout: 5 * time.Second, ResponseHeaderTimeout: 5 * time.Second}
	t.Cleanup(transport.CloseIdleConnections)
	return &http.Client{
		Transport:     emulatorTransport{endpoint: u, base: transport},
		Timeout:       10 * time.Second,
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}, u
}

func TestCrossProtocolObjectsStrict(t *testing.T) {
	grpcEndpoint := crossGRPCEndpoint(t, "GCS_MIMIC_STRICT_GRPC_ENDPOINT")
	ctx, adminClient := newStrictClient(t, strictAdminToken)
	bucket := resumableBucket(t, ctx, adminClient, strictProject)
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	httpClient, endpoint := strictCrossHTTP(t)
	env := newCrossEnv(t, ctx, httpClient, endpoint, grpcEndpoint, bucket.BucketName())
	admin := env.as(strictAdminToken)
	transports := []string{"json", "xml", "grpc"}
	t.Cleanup(func() {
		cleanup, stop := context.WithTimeout(context.Background(), 5*time.Second)
		defer stop()
		for _, transport := range transports {
			_ = bucket.Object("strict-" + transport).Delete(cleanup)
		}
	})
	views := map[string]crossView{}
	listed := map[string][]int64{}
	for _, transport := range transports {
		name := "strict-" + transport
		payload := []byte("private through " + transport)
		generation, err := admin.write(transport, name, payload, nil)
		if err != nil {
			t.Fatalf("admin %s write: %v", transport, err)
		}
		views[name] = expectedView(generation, transport, payload)
		listed[name] = []int64{generation}
	}

	// The same registered principal gets the same decision on every transport for each object.
	viewer := env.as(strictViewerToken)
	for name, view := range views {
		viewer.requireViews("viewer read", name, 0, view)
	}
	viewer.requireListings("viewer listing", false, listed)
	for _, test := range []struct {
		principal string
		env       *crossEnv
		httpCode  int
		xmlCode   string
		grpcCode  codes.Code
	}{
		{"creator without objects.get", env.as(strictCreatorToken), http.StatusForbidden, "AccessDenied", codes.PermissionDenied},
		{"anonymous", env.as(""), http.StatusUnauthorized, "AuthenticationRequired", codes.Unauthenticated},
	} {
		for _, name := range slices.Sorted(maps.Keys(views)) {
			label := test.principal + " read of " + name
			_, err := test.env.viewJSON(name, 0)
			checkHTTPError(t, label+" JSON", err, test.httpCode, "")
			_, _, err = test.env.send(http.MethodGet, test.env.jsonPath(name), url.Values{"alt": {"media"}}, nil, nil)
			checkHTTPError(t, label+" JSON media", err, test.httpCode, "")
			_, err = test.env.viewXML(name, 0)
			checkHTTPError(t, label+" XML", err, test.httpCode, test.xmlCode)
			_, err = test.env.getV2(name, nil)
			checkCode(t, label+" gRPC", err, test.grpcCode)
			_, err = test.env.sdk.Bucket(test.env.bucket).Object(name).NewReader(test.env.grpcContext())
			checkCode(t, label+" gRPC media", err, test.grpcCode)
			_, err = test.env.viewV1(name, 0)
			checkCode(t, label+" gRPC v1", err, test.grpcCode)
		}
		_, _, err := test.env.send(http.MethodGet, "/storage/v1/b/"+test.env.bucket+"/o", nil, nil, nil)
		checkHTTPError(t, test.principal+" JSON listing", err, test.httpCode, "")
		_, err = test.env.listGRPC(false)
		checkCode(t, test.principal+" gRPC listing", err, test.grpcCode)
		_, err = test.env.listV1(false)
		checkCode(t, test.principal+" gRPC v1 listing", err, test.grpcCode)
	}

	// Principals without objects.create cannot overwrite through any transport, and every
	// transport still observes the admin-written generation afterwards.
	for _, test := range []struct {
		principal string
		env       *crossEnv
		httpCode  int
		xmlCode   string
		grpcCode  codes.Code
	}{
		{"viewer", viewer, http.StatusForbidden, "AccessDenied", codes.PermissionDenied},
		{"anonymous", env.as(""), http.StatusUnauthorized, "AuthenticationRequired", codes.Unauthenticated},
	} {
		for _, transport := range transports {
			for _, name := range slices.Sorted(maps.Keys(views)) {
				label := test.principal + " " + transport + " overwrite of " + name
				_, err := test.env.write(transport, name, []byte("denied overwrite"), nil)
				switch transport {
				case "grpc":
					checkCode(t, label, err, test.grpcCode)
				case "xml":
					checkHTTPError(t, label, err, test.httpCode, test.xmlCode)
				default:
					checkHTTPError(t, label, err, test.httpCode, "")
				}
			}
		}
	}
	for name, view := range views {
		admin.requireViews("after denied overwrites", name, 0, view)
	}
}
