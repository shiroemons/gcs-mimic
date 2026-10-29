// Command scorecard runs emulator-neutral Go SDK probes and prints one JSON line
// per case. Every product receives the same cases, inputs and pass criteria, so
// the score only reflects what an ordinary SDK user can observe.
package main

import (
	"bytes"
	"compress/gzip"
	"context"
	"crypto/md5"
	"crypto/rand"
	"encoding/json"
	"errors"
	"fmt"
	"hash/crc32"
	"io"
	"mime/multipart"
	"net/http"
	"net/url"
	"os"
	"sort"
	"strings"
	"time"

	"cloud.google.com/go/iam"
	"cloud.google.com/go/storage"
	"google.golang.org/api/googleapi"
	"google.golang.org/api/iterator"
	"google.golang.org/api/option"
)

type probe struct {
	ctx      context.Context
	client   *storage.Client
	grpc     *storage.Client
	grpcBidi *storage.Client
	endpoint *url.URL
	project  string
	key      []byte
	signer   string
}

type testCase struct {
	id   string
	area string
	run  func(p *probe) error
}

type result struct {
	SDK    string `json:"sdk"`
	ID     string `json:"id"`
	Area   string `json:"area"`
	Pass   bool   `json:"pass"`
	Detail string `json:"detail,omitempty"`
}

func main() {
	endpoint, err := url.Parse(os.Getenv("SCORECARD_ENDPOINT"))
	if err != nil || endpoint.Host == "" {
		fmt.Fprintln(os.Stderr, "SCORECARD_ENDPOINT must be an http(s) origin")
		os.Exit(2)
	}
	key, err := os.ReadFile(envOr("SCORECARD_SIGNING_KEY", "../../fixtures/signing/test-only-private.pem"))
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}
	// The SDK reads the emulator origin from the environment; explicit anonymous
	// clients prevent credential discovery and metadata-server access.
	os.Setenv("STORAGE_EMULATOR_HOST", endpoint.String())
	ctx := context.Background()
	client, err := storage.NewClient(ctx, option.WithoutAuthentication())
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}
	p := &probe{ctx: ctx, client: client, endpoint: endpoint, project: envOr("SCORECARD_PROJECT", "local-project"),
		key: key, signer: "local-admin@example.invalid"}
	if grpcHost := os.Getenv("SCORECARD_GRPC_ENDPOINT"); grpcHost != "" {
		os.Setenv("STORAGE_EMULATOR_HOST_GRPC", grpcHost)
		if p.grpc, err = storage.NewGRPCClient(ctx, option.WithoutAuthentication()); err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(2)
		}
		// MultiRangeDownloader requires bidi reads; a separate client keeps the
		// other gRPC cases on the default read path.
		if p.grpcBidi, err = storage.NewGRPCClient(ctx, option.WithoutAuthentication(), storage.WithGRPCBidiReads()); err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(2)
		}
	}
	filter := os.Getenv("SCORECARD_RUN")
	encoder := json.NewEncoder(os.Stdout)
	for _, c := range cases() {
		if filter != "" && !strings.Contains(c.id, filter) {
			continue
		}
		err := runCase(p, c)
		r := result{SDK: "go", ID: c.id, Area: c.area, Pass: err == nil}
		if err != nil {
			r.Detail = truncate(err.Error(), 300)
		}
		_ = encoder.Encode(r)
	}
}

// runCase bounds every case so that a hanging emulator fails one case instead of the run.
func runCase(p *probe, c testCase) (err error) {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	defer func() {
		if r := recover(); r != nil {
			err = fmt.Errorf("panic: %v", r)
		}
	}()
	child := *p
	child.ctx = ctx
	done := make(chan error, 1)
	go func() {
		defer func() {
			if r := recover(); r != nil {
				done <- fmt.Errorf("panic: %v", r)
			}
		}()
		done <- c.run(&child)
	}()
	select {
	case err = <-done:
		return err
	case <-ctx.Done():
		return fmt.Errorf("timeout")
	}
}

func envOr(name, fallback string) string {
	if v := os.Getenv(name); v != "" {
		return v
	}
	return fallback
}

func truncate(s string, n int) string {
	if len(s) > n {
		return s[:n] + "..."
	}
	return s
}

func uniqueName(prefix string) string {
	var b [6]byte
	_, _ = rand.Read(b[:])
	return fmt.Sprintf("sc-%s-%x", prefix, b)
}

func (p *probe) bucket(attrs *storage.BucketAttrs) (*storage.BucketHandle, error) {
	return p.bucketWith(p.client, attrs)
}

func (p *probe) bucketWith(client *storage.Client, attrs *storage.BucketAttrs) (*storage.BucketHandle, error) {
	b := client.Bucket(uniqueName("go"))
	return b, b.Create(p.ctx, p.project, attrs)
}

func write(ctx context.Context, o *storage.ObjectHandle, data []byte, edit func(*storage.Writer)) (*storage.ObjectAttrs, error) {
	w := o.NewWriter(ctx)
	if edit != nil {
		edit(w)
	}
	if _, err := w.Write(data); err != nil {
		w.Close()
		return nil, err
	}
	if err := w.Close(); err != nil {
		return nil, err
	}
	return w.Attrs(), nil
}

func read(ctx context.Context, o *storage.ObjectHandle) ([]byte, error) {
	r, err := o.NewReader(ctx)
	if err != nil {
		return nil, err
	}
	defer r.Close()
	return io.ReadAll(r)
}

func readRange(ctx context.Context, o *storage.ObjectHandle, offset, length int64) ([]byte, error) {
	r, err := o.NewRangeReader(ctx, offset, length)
	if err != nil {
		return nil, err
	}
	defer r.Close()
	return io.ReadAll(r)
}

func expectBytes(got, want []byte, err error) error {
	if err != nil {
		return err
	}
	if !bytes.Equal(got, want) {
		return fmt.Errorf("bytes differ: got %q want %q", truncate(string(got), 40), truncate(string(want), 40))
	}
	return nil
}

func expectCode(err error, code int) error {
	var apiErr *googleapi.Error
	if errors.As(err, &apiErr) && apiErr.Code == code {
		return nil
	}
	return fmt.Errorf("want HTTP %d, got %v", code, err)
}

func listNames(ctx context.Context, b *storage.BucketHandle, q *storage.Query) ([]string, []string, error) {
	var names, prefixes []string
	it := b.Objects(ctx, q)
	for {
		a, err := it.Next()
		if err == iterator.Done {
			return names, prefixes, nil
		}
		if err != nil {
			return nil, nil, err
		}
		if a.Prefix != "" {
			prefixes = append(prefixes, a.Prefix)
		} else {
			names = append(names, a.Name)
		}
	}
}

func expectList(got []string, want ...string) error {
	sort.Strings(got)
	if strings.Join(got, ",") != strings.Join(want, ",") {
		return fmt.Errorf("list: got %v want %v", got, want)
	}
	return nil
}

func seedObjects(ctx context.Context, b *storage.BucketHandle, names ...string) error {
	for _, n := range names {
		if _, err := write(ctx, b.Object(n), []byte(n), nil); err != nil {
			return err
		}
	}
	return nil
}

func payload(n int) []byte {
	data := make([]byte, n)
	for i := range data {
		data[i] = byte('a' + i%26)
	}
	return data
}

func (p *probe) signedURL(bucket, object, method string, scheme storage.SigningScheme) (string, error) {
	return storage.SignedURL(bucket, object, &storage.SignedURLOptions{
		Scheme: scheme, GoogleAccessID: p.signer, PrivateKey: p.key, Method: method,
		Expires: time.Now().Add(10 * time.Minute), Hostname: p.endpoint.Host,
		Insecure: p.endpoint.Scheme == "http",
	})
}

func httpDo(ctx context.Context, method, target string, body io.Reader, header http.Header) (int, []byte, error) {
	req, err := http.NewRequestWithContext(ctx, method, target, body)
	if err != nil {
		return 0, nil, err
	}
	for k, v := range header {
		req.Header[k] = v
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, nil, err
	}
	defer resp.Body.Close()
	data, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	return resp.StatusCode, data, err
}

func cases() []testCase {
	return []testCase{
		// Buckets.
		{"bucket.create_get_delete", "bucket", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			if _, err := b.Attrs(p.ctx); err != nil {
				return err
			}
			if err := b.Delete(p.ctx); err != nil {
				return err
			}
			if _, err := b.Attrs(p.ctx); !errors.Is(err, storage.ErrBucketNotExist) {
				return fmt.Errorf("deleted bucket: want ErrBucketNotExist, got %v", err)
			}
			return nil
		}},
		{"bucket.list_prefix_paging", "bucket", func(p *probe) error {
			prefix := uniqueName("lst")
			for i := 0; i < 3; i++ {
				if err := p.client.Bucket(fmt.Sprintf("%s-%d", prefix, i)).Create(p.ctx, p.project, nil); err != nil {
					return err
				}
			}
			it := p.client.Buckets(p.ctx, p.project)
			it.Prefix = prefix
			var names []string
			pager := iterator.NewPager(it, 2, "")
			for {
				var page []*storage.BucketAttrs
				token, err := pager.NextPage(&page)
				if err != nil {
					return err
				}
				if len(page) > 2 {
					return fmt.Errorf("page size %d exceeds 2", len(page))
				}
				for _, a := range page {
					names = append(names, a.Name)
				}
				if token == "" {
					break
				}
			}
			return expectList(names, prefix+"-0", prefix+"-1", prefix+"-2")
		}},
		{"bucket.update_labels", "bucket", func(p *probe) error {
			b, err := p.bucket(&storage.BucketAttrs{Labels: map[string]string{"a": "1", "b": "2"}})
			if err != nil {
				return err
			}
			var u storage.BucketAttrsToUpdate
			u.SetLabel("c", "3")
			u.DeleteLabel("a")
			if _, err := b.Update(p.ctx, u); err != nil {
				return err
			}
			a, err := b.Attrs(p.ctx)
			if err != nil {
				return err
			}
			if len(a.Labels) != 2 || a.Labels["b"] != "2" || a.Labels["c"] != "3" {
				return fmt.Errorf("labels %v", a.Labels)
			}
			return nil
		}},
		{"bucket.metageneration_precondition", "bucket", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			_, err = b.If(storage.BucketConditions{MetagenerationMatch: 999}).Update(p.ctx, storage.BucketAttrsToUpdate{StorageClass: "NEARLINE"})
			return expectCode(err, 412)
		}},
		{"bucket.versioning_history", "bucket", func(p *probe) error {
			b, err := p.bucket(&storage.BucketAttrs{VersioningEnabled: true})
			if err != nil {
				return err
			}
			o := b.Object("v.txt")
			first, err := write(p.ctx, o, []byte("one"), nil)
			if err != nil {
				return err
			}
			if _, err := write(p.ctx, o, []byte("two"), nil); err != nil {
				return err
			}
			names, _, err := listNames(p.ctx, b, &storage.Query{Versions: true})
			if err != nil {
				return err
			}
			if len(names) != 2 {
				return fmt.Errorf("versions listed %d, want 2", len(names))
			}
			got, err := read(p.ctx, o.Generation(first.Generation))
			return expectBytes(got, []byte("one"), err)
		}},
		{"bucket.lifecycle", "bucket", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			lc := storage.Lifecycle{Rules: []storage.LifecycleRule{{
				Action:    storage.LifecycleAction{Type: storage.DeleteAction},
				Condition: storage.LifecycleCondition{AgeInDays: 30},
			}}}
			if _, err := b.Update(p.ctx, storage.BucketAttrsToUpdate{Lifecycle: &lc}); err != nil {
				return err
			}
			a, err := b.Attrs(p.ctx)
			if err != nil {
				return err
			}
			if len(a.Lifecycle.Rules) != 1 || a.Lifecycle.Rules[0].Condition.AgeInDays != 30 {
				return fmt.Errorf("lifecycle %+v", a.Lifecycle)
			}
			return nil
		}},
		{"bucket.cors", "bucket", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			cors := []storage.CORS{{Origins: []string{"https://app.example"}, Methods: []string{"GET"}, MaxAge: time.Hour}}
			if _, err := b.Update(p.ctx, storage.BucketAttrsToUpdate{CORS: cors}); err != nil {
				return err
			}
			a, err := b.Attrs(p.ctx)
			if err != nil {
				return err
			}
			if len(a.CORS) != 1 || a.CORS[0].Origins[0] != "https://app.example" {
				return fmt.Errorf("cors %+v", a.CORS)
			}
			return nil
		}},
		{"bucket.retention_policy", "bucket", func(p *probe) error {
			b, err := p.bucket(&storage.BucketAttrs{RetentionPolicy: &storage.RetentionPolicy{RetentionPeriod: time.Hour}})
			if err != nil {
				return err
			}
			if _, err := write(p.ctx, b.Object("r"), []byte("r"), nil); err != nil {
				return err
			}
			return expectCode(b.Object("r").Delete(p.ctx), 403)
		}},
		{"bucket.retention_lock", "bucket", func(p *probe) error {
			b, err := p.bucket(&storage.BucketAttrs{RetentionPolicy: &storage.RetentionPolicy{RetentionPeriod: time.Hour}})
			if err != nil {
				return err
			}
			a, err := b.Attrs(p.ctx)
			if err != nil {
				return err
			}
			if err := b.If(storage.BucketConditions{MetagenerationMatch: a.MetaGeneration}).LockRetentionPolicy(p.ctx); err != nil {
				return err
			}
			a, err = b.Attrs(p.ctx)
			if err != nil {
				return err
			}
			if a.RetentionPolicy == nil || !a.RetentionPolicy.IsLocked {
				return fmt.Errorf("policy not locked: %+v", a.RetentionPolicy)
			}
			return nil
		}},
		{"bucket.uniform_bucket_level_access", "bucket", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			if _, err := b.Update(p.ctx, storage.BucketAttrsToUpdate{UniformBucketLevelAccess: &storage.UniformBucketLevelAccess{Enabled: true}}); err != nil {
				return err
			}
			a, err := b.Attrs(p.ctx)
			if err != nil {
				return err
			}
			if !a.UniformBucketLevelAccess.Enabled {
				return fmt.Errorf("UBLA not enabled")
			}
			return nil
		}},
		{"bucket.public_access_prevention", "bucket", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			if _, err := b.Update(p.ctx, storage.BucketAttrsToUpdate{PublicAccessPrevention: storage.PublicAccessPreventionEnforced}); err != nil {
				return err
			}
			a, err := b.Attrs(p.ctx)
			if err != nil {
				return err
			}
			if a.PublicAccessPrevention != storage.PublicAccessPreventionEnforced {
				return fmt.Errorf("PAP %v", a.PublicAccessPrevention)
			}
			return nil
		}},
		{"bucket.iam_policy", "bucket", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			policy, err := b.IAM().Policy(p.ctx)
			if err != nil {
				return err
			}
			policy.Add("user:someone@example.invalid", iam.RoleName("roles/storage.objectViewer"))
			if err := b.IAM().SetPolicy(p.ctx, policy); err != nil {
				return err
			}
			policy, err = b.IAM().Policy(p.ctx)
			if err != nil {
				return err
			}
			if !policy.HasRole("user:someone@example.invalid", "roles/storage.objectViewer") {
				return fmt.Errorf("binding missing after SetPolicy")
			}
			return nil
		}},
		{"bucket.iam_test_permissions", "bucket", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			perms, err := b.IAM().TestPermissions(p.ctx, []string{"storage.buckets.get", "storage.objects.list"})
			if err != nil {
				return err
			}
			if len(perms) == 0 {
				return fmt.Errorf("no permissions returned")
			}
			return nil
		}},
		{"bucket.soft_delete_policy", "bucket", func(p *probe) error {
			b, err := p.bucket(&storage.BucketAttrs{SoftDeletePolicy: &storage.SoftDeletePolicy{RetentionDuration: 10 * 24 * time.Hour}})
			if err != nil {
				return err
			}
			a, err := b.Attrs(p.ctx)
			if err != nil {
				return err
			}
			if a.SoftDeletePolicy == nil || a.SoftDeletePolicy.RetentionDuration != 10*24*time.Hour {
				return fmt.Errorf("soft delete policy %+v", a.SoftDeletePolicy)
			}
			return nil
		}},
		{"bucket.autoclass", "bucket", func(p *probe) error {
			b, err := p.bucket(&storage.BucketAttrs{Autoclass: &storage.Autoclass{Enabled: true}})
			if err != nil {
				return err
			}
			a, err := b.Attrs(p.ctx)
			if err != nil {
				return err
			}
			if a.Autoclass == nil || !a.Autoclass.Enabled {
				return fmt.Errorf("autoclass %+v", a.Autoclass)
			}
			return nil
		}},
		{"bucket.acl", "bucket", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			if err := b.ACL().Set(p.ctx, storage.AllAuthenticatedUsers, storage.RoleReader); err != nil {
				return err
			}
			rules, err := b.ACL().List(p.ctx)
			if err != nil {
				return err
			}
			for _, r := range rules {
				if r.Entity == storage.AllAuthenticatedUsers && r.Role == storage.RoleReader {
					return nil
				}
			}
			return fmt.Errorf("ACL entry missing: %+v", rules)
		}},
		{"bucket.default_object_acl", "bucket", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			if err := b.DefaultObjectACL().Set(p.ctx, storage.AllAuthenticatedUsers, storage.RoleReader); err != nil {
				return err
			}
			rules, err := b.DefaultObjectACL().List(p.ctx)
			if err != nil {
				return err
			}
			for _, r := range rules {
				if r.Entity == storage.AllAuthenticatedUsers && r.Role == storage.RoleReader {
					return nil
				}
			}
			return fmt.Errorf("default ACL entry missing: %+v", rules)
		}},
		{"bucket.notifications", "bucket", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			n, err := b.AddNotification(p.ctx, &storage.Notification{TopicProjectID: p.project, TopicID: "scorecard", PayloadFormat: storage.JSONPayload})
			if err != nil {
				return err
			}
			all, err := b.Notifications(p.ctx)
			if err != nil {
				return err
			}
			if _, ok := all[n.ID]; !ok {
				return fmt.Errorf("notification %q not listed", n.ID)
			}
			return b.DeleteNotification(p.ctx, n.ID)
		}},
		{"project.hmac_keys", "bucket", func(p *probe) error {
			key, err := p.client.CreateHMACKey(p.ctx, p.project, "sa@example.invalid")
			if err != nil {
				return err
			}
			if key.Secret == "" || key.AccessID == "" {
				return fmt.Errorf("empty key %+v", key)
			}
			h := p.client.HMACKeyHandle(p.project, key.AccessID)
			if _, err := h.Update(p.ctx, storage.HMACKeyAttrsToUpdate{State: storage.Inactive}); err != nil {
				return err
			}
			return h.Delete(p.ctx)
		}},
		{"project.service_account", "bucket", func(p *probe) error {
			email, err := p.client.ServiceAccount(p.ctx, p.project)
			if err != nil {
				return err
			}
			if !strings.Contains(email, "@") {
				return fmt.Errorf("service account %q", email)
			}
			return nil
		}},

		// Objects.
		{"object.write_read_small", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			o := b.Object("dir/日本語 +%?.txt")
			if _, err := write(p.ctx, o, []byte("hello"), nil); err != nil {
				return err
			}
			got, err := read(p.ctx, o)
			return expectBytes(got, []byte("hello"), err)
		}},
		{"object.resumable_large", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			data := payload(3<<20 + 123)
			o := b.Object("big.bin")
			attrs, err := write(p.ctx, o, data, func(w *storage.Writer) { w.ChunkSize = 256 * 1024 })
			if err != nil {
				return err
			}
			if attrs.Size != int64(len(data)) || attrs.CRC32C != crc32.Checksum(data, crc32.MakeTable(crc32.Castagnoli)) {
				return fmt.Errorf("attrs size=%d crc=%d", attrs.Size, attrs.CRC32C)
			}
			got, err := read(p.ctx, o)
			return expectBytes(got, data, err)
		}},
		{"object.metadata_on_write", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			o := b.Object("meta.txt")
			if _, err := write(p.ctx, o, []byte("x"), func(w *storage.Writer) {
				w.ContentType = "text/plain"
				w.CacheControl = "no-cache"
				w.ContentDisposition = "attachment"
				w.ContentLanguage = "ja"
				w.Metadata = map[string]string{"k": "v"}
			}); err != nil {
				return err
			}
			a, err := o.Attrs(p.ctx)
			if err != nil {
				return err
			}
			if a.ContentType != "text/plain" || a.CacheControl != "no-cache" || a.ContentDisposition != "attachment" ||
				a.ContentLanguage != "ja" || a.Metadata["k"] != "v" {
				return fmt.Errorf("attrs %+v", a)
			}
			return nil
		}},
		{"object.checksum_rejected", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			o := b.Object("crc.txt")
			_, err = write(p.ctx, o, []byte("payload"), func(w *storage.Writer) {
				w.SendCRC32C = true
				w.CRC32C = 12345
			})
			if err == nil {
				return fmt.Errorf("wrong CRC32C accepted")
			}
			if _, err := o.Attrs(p.ctx); !errors.Is(err, storage.ErrObjectNotExist) {
				return fmt.Errorf("rejected upload left an object: %v", err)
			}
			return nil
		}},
		{"object.md5_accepted", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			data := []byte("md5 payload")
			sum := md5Sum(data)
			a, err := write(p.ctx, b.Object("md5.txt"), data, func(w *storage.Writer) { w.MD5 = sum })
			if err != nil {
				return err
			}
			if !bytes.Equal(a.MD5, sum) {
				return fmt.Errorf("md5 %x", a.MD5)
			}
			return nil
		}},
		{"object.range_reads", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			o := b.Object("range.txt")
			if _, err := write(p.ctx, o, []byte("0123456789"), nil); err != nil {
				return err
			}
			if err := expectBytesArgs(readRange(p.ctx, o, 2, 4))([]byte("2345")); err != nil {
				return err
			}
			if err := expectBytesArgs(readRange(p.ctx, o, 7, -1))([]byte("789")); err != nil {
				return err
			}
			return expectBytesArgs(readRange(p.ctx, o, -3, -1))([]byte("789"))
		}},
		{"object.update_metadata", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			o := b.Object("u.txt")
			if _, err := write(p.ctx, o, []byte("x"), func(w *storage.Writer) { w.Metadata = map[string]string{"a": "1", "b": "2"} }); err != nil {
				return err
			}
			// PATCH merges supplied keys; the SDK documents an empty map as "delete all".
			a, err := o.Update(p.ctx, storage.ObjectAttrsToUpdate{ContentType: "text/csv", Metadata: map[string]string{"c": "3"}})
			if err != nil {
				return err
			}
			if a.ContentType != "text/csv" || len(a.Metadata) != 3 || a.Metadata["a"] != "1" || a.Metadata["c"] != "3" {
				return fmt.Errorf("merged attrs %+v", a)
			}
			a, err = o.Update(p.ctx, storage.ObjectAttrsToUpdate{Metadata: map[string]string{}})
			if err != nil {
				return err
			}
			if len(a.Metadata) != 0 {
				return fmt.Errorf("metadata not cleared: %v", a.Metadata)
			}
			return nil
		}},
		{"object.delete_not_found", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			o := b.Object("d.txt")
			if _, err := write(p.ctx, o, []byte("x"), nil); err != nil {
				return err
			}
			if err := o.Delete(p.ctx); err != nil {
				return err
			}
			if _, err := read(p.ctx, o); !errors.Is(err, storage.ErrObjectNotExist) {
				return fmt.Errorf("read after delete: %v", err)
			}
			if err := o.Delete(p.ctx); !errors.Is(err, storage.ErrObjectNotExist) {
				return fmt.Errorf("second delete: %v", err)
			}
			return nil
		}},
		{"object.does_not_exist_precondition", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			o := b.Object("once.txt").If(storage.Conditions{DoesNotExist: true})
			if _, err := write(p.ctx, o, []byte("first"), nil); err != nil {
				return err
			}
			_, err = write(p.ctx, o, []byte("second"), nil)
			if err := expectCode(err, 412); err != nil {
				return err
			}
			got, err := read(p.ctx, b.Object("once.txt"))
			return expectBytes(got, []byte("first"), err)
		}},
		{"object.generation_match_precondition", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			o := b.Object("g.txt")
			a, err := write(p.ctx, o, []byte("x"), nil)
			if err != nil {
				return err
			}
			if err := expectCode(o.If(storage.Conditions{GenerationMatch: a.Generation + 1}).Delete(p.ctx), 412); err != nil {
				return err
			}
			return o.If(storage.Conditions{GenerationMatch: a.Generation}).Delete(p.ctx)
		}},
		{"object.metageneration_match_precondition", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			o := b.Object("m.txt")
			a, err := write(p.ctx, o, []byte("x"), nil)
			if err != nil {
				return err
			}
			_, err = o.If(storage.Conditions{MetagenerationMatch: a.Metageneration + 5}).Update(p.ctx, storage.ObjectAttrsToUpdate{ContentType: "text/plain"})
			return expectCode(err, 412)
		}},
		{"object.list_prefix_delimiter", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			if err := seedObjects(p.ctx, b, "a/1", "a/2", "b/1", "c"); err != nil {
				return err
			}
			names, prefixes, err := listNames(p.ctx, b, &storage.Query{Delimiter: "/"})
			if err != nil {
				return err
			}
			if err := expectList(prefixes, "a/", "b/"); err != nil {
				return err
			}
			if err := expectList(names, "c"); err != nil {
				return err
			}
			names, _, err = listNames(p.ctx, b, &storage.Query{Prefix: "a/"})
			if err != nil {
				return err
			}
			return expectList(names, "a/1", "a/2")
		}},
		{"object.list_paging", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			if err := seedObjects(p.ctx, b, "p1", "p2", "p3", "p4", "p5"); err != nil {
				return err
			}
			pager := iterator.NewPager(b.Objects(p.ctx, nil), 2, "")
			var names []string
			pages := 0
			for {
				var page []*storage.ObjectAttrs
				token, err := pager.NextPage(&page)
				if err != nil {
					return err
				}
				pages++
				if len(page) > 2 {
					return fmt.Errorf("page size %d exceeds 2", len(page))
				}
				for _, a := range page {
					names = append(names, a.Name)
				}
				if token == "" {
					break
				}
			}
			if pages < 3 {
				return fmt.Errorf("got %d pages, want 3", pages)
			}
			return expectList(names, "p1", "p2", "p3", "p4", "p5")
		}},
		{"object.list_offsets", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			if err := seedObjects(p.ctx, b, "a", "b", "c", "d"); err != nil {
				return err
			}
			names, _, err := listNames(p.ctx, b, &storage.Query{StartOffset: "b", EndOffset: "d"})
			if err != nil {
				return err
			}
			return expectList(names, "b", "c")
		}},
		{"object.list_match_glob", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			if err := seedObjects(p.ctx, b, "x/a.txt", "x/b.csv", "y/c.txt"); err != nil {
				return err
			}
			names, _, err := listNames(p.ctx, b, &storage.Query{MatchGlob: "**/*.txt"})
			if err != nil {
				return err
			}
			return expectList(names, "x/a.txt", "y/c.txt")
		}},
		{"object.copy_with_metadata", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			dst, err := p.bucket(nil)
			if err != nil {
				return err
			}
			if _, err := write(p.ctx, b.Object("src"), []byte("copy me"), nil); err != nil {
				return err
			}
			c := dst.Object("dst").CopierFrom(b.Object("src"))
			c.ContentType = "text/plain"
			c.Metadata = map[string]string{"copied": "yes"}
			a, err := c.Run(p.ctx)
			if err != nil {
				return err
			}
			if a.ContentType != "text/plain" || a.Metadata["copied"] != "yes" {
				return fmt.Errorf("copy attrs %+v", a)
			}
			got, err := read(p.ctx, dst.Object("dst"))
			return expectBytes(got, []byte("copy me"), err)
		}},
		{"object.rewrite_storage_class", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			data := payload(2<<20 + 7)
			if _, err := write(p.ctx, b.Object("src"), data, nil); err != nil {
				return err
			}
			c := b.Object("dst").CopierFrom(b.Object("src"))
			c.StorageClass = "NEARLINE"
			a, err := c.Run(p.ctx)
			if err != nil {
				return err
			}
			if a.StorageClass != "NEARLINE" {
				return fmt.Errorf("storage class %q", a.StorageClass)
			}
			got, err := read(p.ctx, b.Object("dst"))
			return expectBytes(got, data, err)
		}},
		{"object.compose", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			if err := seedObjects(p.ctx, b, "p1", "p2", "p3"); err != nil {
				return err
			}
			a, err := b.Object("all").ComposerFrom(b.Object("p1"), b.Object("p2"), b.Object("p3")).Run(p.ctx)
			if err != nil {
				return err
			}
			if a.ComponentCount != 3 {
				return fmt.Errorf("componentCount %d", a.ComponentCount)
			}
			got, err := read(p.ctx, b.Object("all"))
			return expectBytes(got, []byte("p1p2p3"), err)
		}},
		{"object.move", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			if _, err := write(p.ctx, b.Object("old"), []byte("moved"), nil); err != nil {
				return err
			}
			if _, err := b.Object("old").Move(p.ctx, storage.MoveObjectDestination{Object: "new"}); err != nil {
				return err
			}
			if _, err := b.Object("old").Attrs(p.ctx); !errors.Is(err, storage.ErrObjectNotExist) {
				return fmt.Errorf("source remains: %v", err)
			}
			got, err := read(p.ctx, b.Object("new"))
			return expectBytes(got, []byte("moved"), err)
		}},
		{"object.acl", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			o := b.Object("acl.txt")
			if _, err := write(p.ctx, o, []byte("x"), nil); err != nil {
				return err
			}
			if err := o.ACL().Set(p.ctx, storage.AllUsers, storage.RoleReader); err != nil {
				return err
			}
			rules, err := o.ACL().List(p.ctx)
			if err != nil {
				return err
			}
			for _, r := range rules {
				if r.Entity == storage.AllUsers && r.Role == storage.RoleReader {
					return nil
				}
			}
			return fmt.Errorf("object ACL entry missing: %+v", rules)
		}},
		{"object.holds", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			o := b.Object("hold.txt")
			if _, err := write(p.ctx, o, []byte("x"), nil); err != nil {
				return err
			}
			if _, err := o.Update(p.ctx, storage.ObjectAttrsToUpdate{TemporaryHold: true}); err != nil {
				return err
			}
			if err := expectCode(o.Delete(p.ctx), 403); err != nil {
				return err
			}
			if _, err := o.Update(p.ctx, storage.ObjectAttrsToUpdate{TemporaryHold: false}); err != nil {
				return err
			}
			return o.Delete(p.ctx)
		}},
		{"object.retention", "object", func(p *probe) error {
			b := p.client.Bucket(uniqueName("go")).SetObjectRetention(true)
			if err := b.Create(p.ctx, p.project, nil); err != nil {
				return err
			}
			o := b.Object("ret.txt")
			if _, err := write(p.ctx, o, []byte("x"), func(w *storage.Writer) {
				w.Retention = &storage.ObjectRetention{Mode: "Unlocked", RetainUntil: time.Now().Add(time.Hour)}
			}); err != nil {
				return err
			}
			a, err := o.Attrs(p.ctx)
			if err != nil {
				return err
			}
			if a.Retention == nil || a.Retention.Mode != "Unlocked" {
				return fmt.Errorf("retention %+v", a.Retention)
			}
			return expectCode(o.Delete(p.ctx), 403)
		}},
		{"object.custom_time", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			o := b.Object("ct.txt")
			if _, err := write(p.ctx, o, []byte("x"), nil); err != nil {
				return err
			}
			want := time.Date(2026, 1, 2, 3, 4, 5, 0, time.UTC)
			a, err := o.Update(p.ctx, storage.ObjectAttrsToUpdate{CustomTime: want})
			if err != nil {
				return err
			}
			if !a.CustomTime.Equal(want) {
				return fmt.Errorf("customTime %v", a.CustomTime)
			}
			return nil
		}},
		{"object.storage_class_on_write", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			a, err := write(p.ctx, b.Object("cold"), []byte("x"), func(w *storage.Writer) { w.StorageClass = "COLDLINE" })
			if err != nil {
				return err
			}
			if a.StorageClass != "COLDLINE" {
				return fmt.Errorf("storage class %q", a.StorageClass)
			}
			return nil
		}},
		{"object.gzip_transcoding", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			plain := []byte(strings.Repeat("compress me ", 50))
			var zipped bytes.Buffer
			zw := gzip.NewWriter(&zipped)
			zw.Write(plain)
			zw.Close()
			o := b.Object("gz.txt")
			if _, err := write(p.ctx, o, zipped.Bytes(), func(w *storage.Writer) {
				w.ContentType = "text/plain"
				w.ContentEncoding = "gzip"
			}); err != nil {
				return err
			}
			if err := expectBytesArgs(read(p.ctx, o))(plain); err != nil {
				return fmt.Errorf("decompressed read: %w", err)
			}
			if err := expectBytesArgs(read(p.ctx, o.ReadCompressed(true)))(zipped.Bytes()); err != nil {
				return fmt.Errorf("compressed read: %w", err)
			}
			return nil
		}},
		{"object.customer_supplied_key", "object", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			key := make([]byte, 32)
			rand.Read(key)
			o := b.Object("csek.txt")
			if _, err := write(p.ctx, o.Key(key), []byte("secret"), nil); err != nil {
				return err
			}
			if _, err := read(p.ctx, o); err == nil {
				return fmt.Errorf("read without key succeeded")
			}
			got, err := read(p.ctx, o.Key(key))
			return expectBytes(got, []byte("secret"), err)
		}},
		{"object.soft_delete_restore", "object", func(p *probe) error {
			b, err := p.bucket(&storage.BucketAttrs{SoftDeletePolicy: &storage.SoftDeletePolicy{RetentionDuration: 7 * 24 * time.Hour}})
			if err != nil {
				return err
			}
			o := b.Object("sd.txt")
			a, err := write(p.ctx, o, []byte("restore me"), nil)
			if err != nil {
				return err
			}
			if err := o.Delete(p.ctx); err != nil {
				return err
			}
			names, _, err := listNames(p.ctx, b, &storage.Query{SoftDeleted: true})
			if err != nil {
				return err
			}
			if err := expectList(names, "sd.txt"); err != nil {
				return err
			}
			if _, err := o.Generation(a.Generation).Restore(p.ctx, &storage.RestoreOptions{}); err != nil {
				return err
			}
			got, err := read(p.ctx, o)
			return expectBytes(got, []byte("restore me"), err)
		}},
		{"signed.v4_get", "signed", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			if _, err := write(p.ctx, b.Object("s.txt"), []byte("signed"), nil); err != nil {
				return err
			}
			u, err := p.signedURL(b.BucketName(), "s.txt", "GET", storage.SigningSchemeV4)
			if err != nil {
				return err
			}
			code, body, err := httpDo(p.ctx, "GET", u, nil, nil)
			if err != nil || code != 200 {
				return fmt.Errorf("status %d err %v body %s", code, err, truncate(string(body), 200))
			}
			return expectBytes(body, []byte("signed"), nil)
		}},
		{"signed.v4_put", "signed", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			u, err := p.signedURL(b.BucketName(), "put.txt", "PUT", storage.SigningSchemeV4)
			if err != nil {
				return err
			}
			code, body, err := httpDo(p.ctx, "PUT", u, strings.NewReader("uploaded"), nil)
			if err != nil || code != 200 {
				return fmt.Errorf("status %d err %v body %s", code, err, truncate(string(body), 200))
			}
			got, err := read(p.ctx, b.Object("put.txt"))
			return expectBytes(got, []byte("uploaded"), err)
		}},
		{"signed.v2_get", "signed", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			if _, err := write(p.ctx, b.Object("s2.txt"), []byte("signed v2"), nil); err != nil {
				return err
			}
			u, err := p.signedURL(b.BucketName(), "s2.txt", "GET", storage.SigningSchemeV2)
			if err != nil {
				return err
			}
			// The SDK always emits https for V2; the V2 string-to-sign excludes the
			// scheme and host, so matching the emulator's scheme keeps the signature valid.
			if p.endpoint.Scheme == "http" {
				u = "http" + strings.TrimPrefix(u, "https")
			}
			code, body, err := httpDo(p.ctx, "GET", u, nil, nil)
			if err != nil || code != 200 {
				return fmt.Errorf("status %d err %v body %s", code, err, truncate(string(body), 200))
			}
			return expectBytes(body, []byte("signed v2"), nil)
		}},
		{"signed.post_policy_v4", "signed", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			policy, err := storage.GenerateSignedPostPolicyV4(b.BucketName(), "post.txt", &storage.PostPolicyV4Options{
				GoogleAccessID: p.signer, PrivateKey: p.key, Expires: time.Now().Add(10 * time.Minute),
				Insecure: p.endpoint.Scheme == "http", Hostname: p.endpoint.Host, Style: storage.PathStyle(),
			})
			if err != nil {
				return err
			}
			var form bytes.Buffer
			mw := multipart.NewWriter(&form)
			for k, v := range policy.Fields {
				mw.WriteField(k, v)
			}
			fw, _ := mw.CreateFormFile("file", "post.txt")
			fw.Write([]byte("posted"))
			mw.Close()
			code, body, err := httpDo(p.ctx, "POST", policy.URL, &form, http.Header{"Content-Type": {mw.FormDataContentType()}})
			if err != nil || code/100 != 2 {
				return fmt.Errorf("status %d err %v body %s", code, err, truncate(string(body), 200))
			}
			got, err := read(p.ctx, b.Object("post.txt"))
			return expectBytes(got, []byte("posted"), err)
		}},

		// gRPC transport (storage.NewGRPCClient).
		{"grpc.bucket_create_get_delete", "grpc", func(p *probe) error {
			if p.grpc == nil {
				return fmt.Errorf("no gRPC endpoint")
			}
			b, err := p.bucketWith(p.grpc, nil)
			if err != nil {
				return err
			}
			if _, err := b.Attrs(p.ctx); err != nil {
				return err
			}
			return b.Delete(p.ctx)
		}},
		{"grpc.write_read", "grpc", func(p *probe) error {
			if p.grpc == nil {
				return fmt.Errorf("no gRPC endpoint")
			}
			b, err := p.bucketWith(p.grpc, nil)
			if err != nil {
				return err
			}
			data := payload(1<<20 + 5)
			o := b.Object("g.bin")
			if _, err := write(p.ctx, o, data, func(w *storage.Writer) { w.ChunkSize = 256 * 1024 }); err != nil {
				return err
			}
			got, err := read(p.ctx, o)
			if err := expectBytes(got, data, err); err != nil {
				return err
			}
			return expectBytesArgs(readRange(p.ctx, o, 10, 5))(data[10:15])
		}},
		{"grpc.list", "grpc", func(p *probe) error {
			if p.grpc == nil {
				return fmt.Errorf("no gRPC endpoint")
			}
			b, err := p.bucketWith(p.grpc, nil)
			if err != nil {
				return err
			}
			if err := seedObjects(p.ctx, b, "a/1", "a/2", "b"); err != nil {
				return err
			}
			names, prefixes, err := listNames(p.ctx, b, &storage.Query{Delimiter: "/"})
			if err != nil {
				return err
			}
			if err := expectList(prefixes, "a/"); err != nil {
				return err
			}
			return expectList(names, "b")
		}},
		{"grpc.update_delete", "grpc", func(p *probe) error {
			if p.grpc == nil {
				return fmt.Errorf("no gRPC endpoint")
			}
			b, err := p.bucketWith(p.grpc, nil)
			if err != nil {
				return err
			}
			o := b.Object("u")
			if _, err := write(p.ctx, o, []byte("x"), nil); err != nil {
				return err
			}
			a, err := o.Update(p.ctx, storage.ObjectAttrsToUpdate{Metadata: map[string]string{"k": "v"}})
			if err != nil {
				return err
			}
			if a.Metadata["k"] != "v" {
				return fmt.Errorf("metadata %v", a.Metadata)
			}
			if err := o.Delete(p.ctx); err != nil {
				return err
			}
			if _, err := o.Attrs(p.ctx); !errors.Is(err, storage.ErrObjectNotExist) {
				return fmt.Errorf("after delete: %v", err)
			}
			return nil
		}},
		{"grpc.multi_range_downloader", "grpc", func(p *probe) error {
			if p.grpc == nil {
				return fmt.Errorf("no gRPC endpoint")
			}
			b, err := p.bucketWith(p.grpc, nil)
			if err != nil {
				return err
			}
			if _, err := write(p.ctx, b.Object("mrd"), []byte("0123456789"), nil); err != nil {
				return err
			}
			mrd, err := p.grpcBidi.Bucket(b.BucketName()).Object("mrd").NewMultiRangeDownloader(p.ctx)
			if err != nil {
				return err
			}
			var first, second bytes.Buffer
			var errs []error
			cb := func(_, _ int64, err error) {
				if err != nil {
					errs = append(errs, err)
				}
			}
			mrd.Add(&first, 0, 3, cb)
			mrd.Add(&second, 6, 4, cb)
			mrd.Wait()
			if err := mrd.Close(); err != nil {
				return err
			}
			if len(errs) > 0 {
				return errs[0]
			}
			if first.String() != "012" || second.String() != "6789" {
				return fmt.Errorf("ranges %q %q", first.String(), second.String())
			}
			return nil
		}},
		{"grpc.appendable_object", "grpc", func(p *probe) error {
			if p.grpc == nil {
				return fmt.Errorf("no gRPC endpoint")
			}
			b, err := p.bucketWith(p.grpc, nil)
			if err != nil {
				return err
			}
			o := b.Object("append")
			if _, err := write(p.ctx, o, []byte("appended"), func(w *storage.Writer) { w.Append = true }); err != nil {
				return err
			}
			got, err := read(p.ctx, o)
			return expectBytes(got, []byte("appended"), err)
		}},
	}
}

func md5Sum(data []byte) []byte {
	sum := md5.Sum(data)
	return sum[:]
}

// expectBytesArgs adapts a (data, err) pair for comparison with an expected value.
func expectBytesArgs(got []byte, err error) func([]byte) error {
	return func(want []byte) error { return expectBytes(got, want, err) }
}
