package fakestorage

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"mime/multipart"
	"net/http"
	"net/textproto"
	"os"
	"strconv"
	"strings"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/googleapi"
	"google.golang.org/api/iterator"
)

// ErrUnsupportedFixture prevents unsupported fixture fields from being silently discarded.
var ErrUnsupportedFixture = errors.New("deleted, etag and size fixture injection is unsupported")

type ObjectAttrs struct {
	BucketName         string                   `json:"bucket"`
	Name               string                   `json:"name"`
	Size               int64                    `json:"size,string"`
	StorageClass       string                   `json:"storageClass"`
	ContentType        string                   `json:"contentType"`
	ContentEncoding    string                   `json:"contentEncoding"`
	ContentDisposition string                   `json:"contentDisposition"`
	ContentLanguage    string                   `json:"contentLanguage"`
	CacheControl       string                   `json:"cacheControl"`
	Crc32c             string                   `json:"crc32c,omitempty"`
	Md5Hash            string                   `json:"md5Hash,omitempty"`
	Etag               string                   `json:"etag,omitempty"`
	ACL                []storage.ACLRule        `json:"-"`
	Created            time.Time                `json:"created,omitempty"`
	Updated            time.Time                `json:"updated,omitempty"`
	Deleted            time.Time                `json:"deleted,omitempty"`
	CustomTime         time.Time                `json:"customTime,omitempty"`
	Generation         int64                    `json:"generation,omitempty,string"`
	Metadata           map[string]string        `json:"metadata,omitempty"`
	Retention          *storage.ObjectRetention `json:"-"`
}

type Object struct {
	ObjectAttrs
	Content []byte `json:"-"`
}

type StreamingObject struct {
	ObjectAttrs
	Content io.ReadSeekCloser `json:"-"`
}

type bytesContent struct{ *bytes.Reader }

func (bytesContent) Close() error { return nil }

func (o Object) StreamingObject() StreamingObject {
	return StreamingObject{ObjectAttrs: o.ObjectAttrs, Content: bytesContent{bytes.NewReader(o.Content)}}
}

func (o *StreamingObject) Close() error {
	if o != nil && o.Content != nil {
		return o.Content.Close()
	}
	return nil
}

// BufferedObject is deliberately bounded; use Content for larger fixtures.
func (o *StreamingObject) BufferedObject() (Object, error) {
	data, err := io.ReadAll(io.LimitReader(o.Content, (16<<20)+1))
	if len(data) > 16<<20 {
		return Object{}, errors.New("buffered object exceeds 16 MiB; use StreamingObject.Content")
	}
	return Object{ObjectAttrs: o.ObjectAttrs, Content: data}, err
}

func validateFixture(attrs ObjectAttrs) error {
	if !attrs.Deleted.IsZero() || attrs.Etag != "" || attrs.Size != 0 {
		return ErrUnsupportedFixture
	}
	if attrs.Generation < 0 || attrs.Generation == int64(^uint64(0)>>1) {
		return errors.New("fixture generation must be positive and less than MaxInt64, or zero for allocation")
	}
	return nil
}

func (s *Server) CreateObject(object Object) {
	if err := s.CreateObjectStreaming(object.StreamingObject()); err != nil {
		panic(err)
	}
}

func (s *Server) CreateObjectStreaming(object StreamingObject) error {
	return s.createObjectStreaming(context.Background(), object, runtimeFixture)
}

type fixtureMode uint8

const (
	runtimeFixture fixtureMode = iota
	initialFixture
)

func (s *Server) createObjectStreaming(ctx context.Context, object StreamingObject, mode fixtureMode) error {
	if err := validateFixture(object.ObjectAttrs); err != nil {
		return err
	}
	if object.Content == nil {
		return errors.New("StreamingObject.Content must not be nil")
	}
	body, contentType, err := fixtureBody(object, mode)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	if err := s.ensureFixtureBucket(ctx, object.ObjectAttrs); err != nil {
		return err
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, s.endpoint.String()+"/_internal/objects", body)
	if err != nil {
		return err
	}
	request.Header.Set("Content-Type", contentType)
	response, err := s.HTTPClient().Do(request)
	if err != nil {
		return err
	}
	err = googleapi.CheckResponse(response)
	_, readErr := io.Copy(io.Discard, io.LimitReader(response.Body, (64<<10)+1))
	return errors.Join(err, readErr, response.Body.Close())
}

func (s *Server) ensureFixtureBucket(ctx context.Context, attrs ObjectAttrs) error {
	client := s.Client()
	defer client.Close()
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	bucket := client.Bucket(attrs.BucketName)
	if attrs.Retention != nil {
		bucket = bucket.SetObjectRetention(true)
	}
	if err := bucket.Create(ctx, "local-project", nil); err != nil {
		var api *googleapi.Error
		if !errors.As(err, &api) || api.Code != 409 {
			return fmt.Errorf("ensure fixture bucket: %w", err)
		}
	}
	return nil
}

func fixtureBody(object StreamingObject, mode fixtureMode) (io.Reader, string, error) {
	metadata := map[string]any{"bucket": object.BucketName, "name": object.Name}
	if mode == initialFixture {
		metadata["notificationMode"] = "suppress"
	}
	for key, value := range map[string]string{"contentType": object.ContentType, "contentEncoding": object.ContentEncoding,
		"contentDisposition": object.ContentDisposition, "contentLanguage": object.ContentLanguage,
		"cacheControl": object.CacheControl, "storageClass": object.StorageClass,
		"md5Hash": object.Md5Hash, "crc32c": object.Crc32c} {
		if value != "" {
			metadata[key] = value
		}
	}
	for key, value := range map[string]time.Time{"timeCreated": object.Created, "updated": object.Updated, "customTime": object.CustomTime} {
		if !value.IsZero() {
			metadata[key] = value.Format(time.RFC3339Nano)
		}
	}
	if object.Generation != 0 {
		metadata["generation"] = strconv.FormatInt(object.Generation, 10)
	}
	if object.Metadata != nil {
		metadata["metadata"] = object.Metadata
	}
	if object.ACL != nil {
		acl := encodeACL(object.ACL)
		if acl == nil {
			acl = []aclJSON{}
		}
		metadata["acl"] = acl
	}
	if object.Retention != nil {
		metadata["retention"] = retentionJSON{Mode: object.Retention.Mode, RetainUntil: object.Retention.RetainUntil}
	}
	encoded, err := json.Marshal(metadata)
	if err != nil {
		return nil, "", err
	}
	if len(encoded) > 64<<10 {
		return nil, "", errors.New("fixture metadata exceeds 64 KiB")
	}
	var prefix bytes.Buffer
	writer := multipart.NewWriter(&prefix)
	part, err := writer.CreatePart(textproto.MIMEHeader{"Content-Type": {"application/json"}})
	if err != nil {
		return nil, "", err
	}
	if _, err := part.Write(encoded); err != nil {
		return nil, "", err
	}
	if _, err := writer.CreatePart(textproto.MIMEHeader{"Content-Type": {"application/octet-stream"}}); err != nil {
		return nil, "", err
	}
	boundary := writer.Boundary()
	return io.MultiReader(bytes.NewReader(prefix.Bytes()), object.Content,
		strings.NewReader("\r\n--"+boundary+"--\r\n")), "multipart/related; boundary=" + boundary, nil
}

func (s *Server) GetObject(bucketName, objectName string) (Object, error) {
	return s.GetObjectWithGeneration(bucketName, objectName, 0)
}

func (s *Server) GetObjectWithGeneration(bucketName, objectName string, generation int64) (Object, error) {
	object, err := s.GetObjectWithGenerationStreaming(bucketName, objectName, generation)
	if err != nil {
		return Object{}, err
	}
	buffered, err := object.BufferedObject()
	return buffered, errors.Join(err, object.Close())
}

func (s *Server) GetObjectStreaming(bucketName, objectName string) (StreamingObject, error) {
	return s.GetObjectWithGenerationStreaming(bucketName, objectName, 0)
}

func (s *Server) GetObjectWithGenerationStreaming(bucketName, objectName string, generation int64) (StreamingObject, error) {
	client := s.Client()
	defer client.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	object := client.Bucket(bucketName).Object(objectName)
	if generation != 0 {
		object = object.Generation(generation)
	}
	attrs, err := object.Attrs(ctx)
	if err != nil {
		return StreamingObject{}, err
	}
	reader, err := object.Generation(attrs.Generation).NewReader(ctx)
	if err != nil {
		return StreamingObject{}, err
	}
	file, err := os.CreateTemp("", "gcs-mimic-go-read-")
	if err != nil {
		return StreamingObject{}, errors.Join(err, reader.Close())
	}
	content := &temporaryContent{File: file}
	_, copyErr := io.Copy(file, reader)
	_, seekErr := file.Seek(0, io.SeekStart)
	if err := errors.Join(copyErr, seekErr, reader.Close()); err != nil {
		return StreamingObject{}, errors.Join(err, content.Close())
	}
	return StreamingObject{ObjectAttrs: fromAttrs(attrs), Content: content}, nil
}

type temporaryContent struct{ *os.File }

func (c *temporaryContent) Close() error {
	return errors.Join(c.File.Close(), os.Remove(c.File.Name()))
}

func fromAttrs(a *storage.ObjectAttrs) ObjectAttrs {
	var checksum [4]byte
	binary.BigEndian.PutUint32(checksum[:], a.CRC32C)
	return ObjectAttrs{BucketName: a.Bucket, Name: a.Name, Size: a.Size, StorageClass: a.StorageClass,
		ContentType: a.ContentType, ContentEncoding: a.ContentEncoding, ContentDisposition: a.ContentDisposition,
		ContentLanguage: a.ContentLanguage, CacheControl: a.CacheControl, Generation: a.Generation,
		Created: a.Created, Updated: a.Updated, Deleted: a.Deleted, CustomTime: a.CustomTime,
		Metadata: a.Metadata, ACL: a.ACL, Retention: a.Retention, Etag: a.Etag,
		Crc32c: base64.StdEncoding.EncodeToString(checksum[:]), Md5Hash: base64.StdEncoding.EncodeToString(a.MD5)}
}

func (s *Server) ListObjects(bucketName, prefix, delimiter string, versions bool) ([]ObjectAttrs, []string, error) {
	return s.ListObjectsWithOptions(bucketName, ListOptions{Prefix: prefix, Delimiter: delimiter, Versions: versions})
}

type ListOptions struct {
	Prefix, Delimiter, StartOffset, EndOffset, PageToken string
	Versions, IncludeTrailingDelimiter                   bool
	MaxResults                                           int
}

type ListResponse struct {
	Objects       []ObjectAttrs
	Prefixes      []string
	NextPageToken string
}

func (s *Server) ListObjectsWithOptions(bucketName string, options ListOptions) ([]ObjectAttrs, []string, error) {
	page, err := s.ListObjectsWithOptionsPaginated(bucketName, options)
	if err == nil && page.NextPageToken != "" && options.MaxResults == 0 {
		return nil, nil, errors.New("listing exceeds 1000 entries; use ListObjectsWithOptionsPaginated")
	}
	return page.Objects, page.Prefixes, err
}

func (s *Server) ListObjectsWithOptionsPaginated(bucketName string, options ListOptions) (ListResponse, error) {
	if options.MaxResults < 0 || options.MaxResults > 1000 {
		return ListResponse{}, errors.New("MaxResults must be between 0 and 1000")
	}
	client := s.Client()
	defer client.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	items := client.Bucket(bucketName).Objects(ctx, &storage.Query{Prefix: options.Prefix, Delimiter: options.Delimiter,
		Versions: options.Versions, StartOffset: options.StartOffset, EndOffset: options.EndOffset,
		IncludeTrailingDelimiter: options.IncludeTrailingDelimiter})
	size := options.MaxResults
	if size == 0 {
		size = 1000
	}
	var entries []*storage.ObjectAttrs
	token, err := iterator.NewPager(items, size, options.PageToken).NextPage(&entries)
	if err != nil {
		return ListResponse{}, err
	}
	page := ListResponse{NextPageToken: token}
	for _, attrs := range entries {
		if attrs.Prefix != "" {
			page.Prefixes = append(page.Prefixes, attrs.Prefix)
		} else {
			page.Objects = append(page.Objects, fromAttrs(attrs))
		}
	}
	return page, nil
}
