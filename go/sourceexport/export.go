package sourceexport

import (
	"context"
	"crypto/md5"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"hash"
	"hash/crc32"
	"io"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"sync"
	"time"
	"unicode/utf8"

	"github.com/fsouza/fake-gcs-server/fakestorage"
)

const SourceRevision = "896ece51e49eed0946089f3089f6343855c0d106"

// PublishedError means the complete destination is visible but its final parent
// sync failed. Retain and verify that capture before retrying to a different path.
type PublishedError struct {
	Destination string
	Err         error
}

func (e *PublishedError) Error() string {
	return fmt.Sprintf("complete snapshot published at %q but directory durability is unconfirmed; verify it before retrying: %v", e.Destination, e.Err)
}

func (e *PublishedError) Unwrap() error { return e.Err }

// The source API allocates bucket and per-bucket metadata lists itself. These
// caps bound accepted captures; they cannot bound an oversized source's List call.
const maxBuckets, maxGenerations = 1000, 10000
const maxObjectBytes int64 = 1 << 30

type capture struct {
	ctx      context.Context
	root     string
	manifest io.Writer
	hash     hash.Hash
	buckets  uint64
	objects  uint64
	bytes    uint64
	openBlob func(string) (durableBlob, error)
}

type durableBlob interface {
	io.Writer
	Sync() error
	Close() error
}

// Export takes the application's exclusive barrier until the complete snapshot
// is published. Every HTTP and direct mutation, including background jobs, must
// hold the same barrier's RLock; an uncooperative server cannot be captured.
// The destination must not exist. Failures retain the source unchanged;
// PublishedError distinguishes an already visible complete capture. Cancellation
// is checked after acquiring the application lock.
func Export(ctx context.Context, source *fakestorage.Server, barrier *sync.RWMutex, destination string) (result error) {
	if source == nil || barrier == nil || destination == "" {
		return errors.New("source, application barrier and a new destination are required")
	}
	if fmt.Sprintf("%T", source.Backend()) != "*backend.storageMemory" {
		return errors.New("only the pinned source memory backend is supported; retain the original storage")
	}
	barrier.Lock()
	defer barrier.Unlock()
	if err := ctx.Err(); err != nil {
		return err
	}
	if _, err := os.Lstat(destination); !errors.Is(err, os.ErrNotExist) {
		if err != nil {
			return err
		}
		return errors.New("snapshot destination already exists")
	}
	stage, err := os.MkdirTemp(filepath.Dir(destination), ".gcs-memory-capture-")
	if err != nil {
		return err
	}
	defer func() { result = errors.Join(result, os.RemoveAll(stage)) }()
	if err := os.Mkdir(filepath.Join(stage, "blobs"), 0700); err != nil {
		return err
	}
	file, err := os.OpenFile(filepath.Join(stage, "snapshot.jsonl"), os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return err
	}
	c := capture{ctx: ctx, root: stage, manifest: file, hash: sha256.New()}
	err = c.write(source)
	if err == nil {
		err = file.Sync()
	}
	if err = errors.Join(err, file.Close()); err != nil {
		return err
	}
	for _, directory := range []string{filepath.Join(stage, "blobs"), stage} {
		if err := syncDirectory(directory); err != nil {
			return err
		}
	}
	if err := ctx.Err(); err != nil {
		return err
	}
	return publishSnapshot(stage, destination, syncDirectory)
}

func publishSnapshot(stage, destination string, syncParent func(string) error) error {
	if err := publish(stage, destination); err != nil {
		return err
	}
	if err := syncParent(filepath.Dir(destination)); err != nil {
		return &PublishedError{Destination: destination, Err: err}
	}
	return nil
}

func syncDirectory(path string) error {
	file, err := os.Open(path)
	if err != nil {
		return err
	}
	return errors.Join(file.Sync(), file.Close())
}

func (c *capture) row(value any, hashed bool) error {
	if err := c.ctx.Err(); err != nil {
		return err
	}
	data, err := json.Marshal(value)
	if err != nil {
		return err
	}
	if len(data)+1 > 128<<10 {
		return errors.New("snapshot record exceeds 128 KiB")
	}
	data = append(data, '\n')
	writer := c.manifest
	if hashed {
		writer = io.MultiWriter(writer, c.hash)
	}
	_, err = writer.Write(data)
	return err
}

func attributes(value any) (json.RawMessage, error) {
	remaining := 64 << 10
	if !validText(reflect.ValueOf(value), &remaining) {
		if remaining < 0 {
			return nil, errors.New("source attributes exceed 64 KiB")
		}
		return nil, errors.New("source attributes contain invalid UTF-8; JSON replacement would lose source state")
	}
	data, err := json.Marshal(value)
	if len(data) > 64<<10 {
		return nil, errors.New("source attributes exceed 64 KiB")
	}
	return data, err
}

func validText(value reflect.Value, remaining *int) bool {
	// The pinned attribute types have no custom marshaler. A lower-bound byte/
	// element budget bounds work before encoding, including arrays of empty values.
	*remaining -= 1
	if *remaining < 0 {
		return false
	}
	switch value.Kind() {
	case reflect.String:
		*remaining -= len(value.String())
		return *remaining >= 0 && utf8.ValidString(value.String())
	case reflect.Pointer, reflect.Interface:
		return value.IsNil() || validText(value.Elem(), remaining)
	case reflect.Struct:
		for index := 0; index < value.NumField(); index++ {
			field := value.Type().Field(index)
			if field.PkgPath != "" || field.Tag.Get("json") == "-" {
				continue
			}
			if !validText(value.Field(index), remaining) {
				return false
			}
		}
	case reflect.Slice, reflect.Array:
		for index := 0; index < value.Len(); index++ {
			if !validText(value.Index(index), remaining) {
				return false
			}
		}
	case reflect.Map:
		items := value.MapRange()
		for items.Next() {
			if !validText(items.Key(), remaining) || !validText(items.Value(), remaining) {
				return false
			}
		}
	}
	return true
}

func (c *capture) write(source *fakestorage.Server) error {
	var id [16]byte
	if _, err := rand.Read(id[:]); err != nil {
		return err
	}
	if err := c.row(map[string]any{"kind": "snapshot", "version": 1, "source_revision": SourceRevision,
		"capture_id": hex.EncodeToString(id[:]), "quiesced": true, "backend": "memory", "original_names_verified": true}, true); err != nil {
		return err
	}
	buckets, err := source.Backend().ListBuckets()
	if err != nil {
		return err
	}
	if len(buckets) > maxBuckets {
		return errors.New("source exceeds 1000 buckets; capture a smaller owned instance")
	}
	sort.Slice(buckets, func(i, j int) bool { return buckets[i].Name < buckets[j].Name })
	for _, bucket := range buckets {
		if len(bucket.Name) > 222 || !utf8.ValidString(bucket.Name) {
			return errors.New("source bucket name exceeds 222 UTF-8 bytes or contains invalid UTF-8")
		}
		objects, err := source.Backend().ListObjects(bucket.Name, "", true)
		if err != nil {
			return err
		}
		if uint64(len(objects))+c.objects > maxGenerations {
			return errors.New("source exceeds 10000 generations; capture a smaller owned instance")
		}
		live, err := source.Backend().ListObjects(bucket.Name, "", false)
		if err != nil {
			return err
		}
		members := make(map[string]int64, len(live))
		for _, object := range live {
			if _, exists := members[object.Name]; exists {
				return errors.New("source has duplicate live names")
			}
			members[object.Name] = object.Generation
		}
		isLive := func(index int) bool { return members[objects[index].Name] == objects[index].Generation }
		sort.Slice(objects, func(i, j int) bool {
			if objects[i].Name != objects[j].Name {
				return objects[i].Name < objects[j].Name
			}
			if isLive(i) != isLive(j) {
				return !isLive(i)
			}
			return objects[i].Generation < objects[j].Generation
		})
		retention := false
		for _, object := range objects {
			retention = retention || object.RetentionMode != "" || object.RetentionRetainUntil != ""
		}
		attrs, err := attributes(map[string]any{"VersioningEnabled": bucket.VersioningEnabled,
			"DefaultEventBasedHold": bucket.DefaultEventBasedHold, "ACL": bucket.ACL})
		if err != nil {
			return err
		}
		if err := c.row(map[string]any{"kind": "bucket", "name": bucket.Name, "created": bucket.TimeCreated.Format(time.RFC3339Nano),
			"attributes": attrs, "enable_object_retention": retention}, true); err != nil {
			return err
		}
		c.buckets++
		for index, object := range objects {
			if len(object.Name) > 1024 || !utf8.ValidString(object.Name) {
				return errors.New("source object name exceeds 1024 UTF-8 bytes or contains invalid UTF-8")
			}
			if object.Generation <= 0 || object.Size < 0 || object.Size > maxObjectBytes ||
				(index > 0 && object.Name == objects[index-1].Name && object.Generation == objects[index-1].Generation) {
				return errors.New("source has an invalid, duplicate or oversized generation")
			}
			membership := "archived"
			if isLive(index) {
				membership = "live"
			}
			deleted, err := sourceTime(object.Deleted)
			if err != nil || (membership == "live") != deleted.IsZero() {
				return errors.New("source live/archive membership disagrees with its exact Deleted timestamp")
			}
			selected, err := source.Backend().GetObjectWithGeneration(bucket.Name, object.Name, object.Generation)
			if err != nil {
				return err
			}
			attrs, err := attributes(selected.ObjectAttrs)
			if err == nil {
				listed, listErr := attributes(object)
				err = listErr
				if err == nil && string(attrs) != string(listed) {
					err = errors.New("selected generation metadata differs from the frozen inventory")
				}
			}
			if err == nil {
				err = c.object(bucket.Name, object.Name, membership, object.Size, attrs, selected.Content, object.Md5Hash, object.Crc32c)
			}
			if err = errors.Join(err, selected.Close()); err != nil {
				return err
			}
		}
	}
	return c.row(map[string]any{"kind": "complete", "buckets": c.buckets, "generations": c.objects,
		"bytes": c.bytes, "manifest_sha256": hex.EncodeToString(c.hash.Sum(nil))}, false)
}

func sourceTime(value string) (time.Time, error) {
	if value == "" {
		return time.Time{}, nil
	}
	return time.Parse(time.RFC3339Nano, value)
}

func (c *capture) object(bucket, name, membership string, size int64, attrs json.RawMessage, body io.Reader, md5Hash, crc string) error {
	blob := fmt.Sprintf("%016x", c.objects+1)
	open := c.openBlob
	if open == nil {
		open = func(name string) (durableBlob, error) {
			return os.OpenFile(name, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
		}
	}
	file, err := open(filepath.Join(c.root, "blobs", blob))
	if err != nil {
		return err
	}
	md5sum := md5.New()
	crcSum := crc32.New(crc32.MakeTable(crc32.Castagnoli))
	written, err := io.CopyBuffer(contextWriter{c.ctx, io.MultiWriter(file, md5sum, crcSum)},
		io.LimitReader(body, size+1), make([]byte, 64<<10))
	if err == nil && written != size {
		err = errors.New("selected generation body length differs from its source size")
	}
	var crcBytes [4]byte
	binary.BigEndian.PutUint32(crcBytes[:], crcSum.Sum32())
	if err == nil && (base64.StdEncoding.EncodeToString(md5sum.Sum(nil)) != md5Hash ||
		base64.StdEncoding.EncodeToString(crcBytes[:]) != crc) {
		err = errors.New("selected generation body differs from source MD5 or CRC32C; retain the source")
	}
	if err == nil {
		err = file.Sync()
	}
	if err = errors.Join(err, file.Close()); err != nil {
		return err
	}
	if err := c.row(map[string]any{"kind": "object", "bucket": bucket, "name": name, "membership": membership,
		"blob": blob, "size": size, "attributes": attrs}, true); err != nil {
		return err
	}
	c.objects++
	c.bytes += uint64(size)
	return nil
}

type contextWriter struct {
	ctx    context.Context
	writer io.Writer
}

func (w contextWriter) Write(data []byte) (int, error) {
	if err := w.ctx.Err(); err != nil {
		return 0, err
	}
	return w.writer.Write(data)
}
