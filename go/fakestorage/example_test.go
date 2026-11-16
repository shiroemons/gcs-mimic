package fakestorage_test

import (
	"context"
	"fmt"
	"io"
	"log"

	"github.com/shiroemons/gcs-mimic/go/fakestorage"
)

// Code written for fake-gcs-server's fakestorage keeps working once its import path names this
// package. The example is compiled but not run: running it needs a gcs-mimic binary, and the
// package's ordinary tests use a stand-in child instead.
func ExampleNewServerWithOptions() {
	server, err := fakestorage.NewServerWithOptions(fakestorage.Options{
		InitialObjects: []fakestorage.Object{{
			ObjectAttrs: fakestorage.ObjectAttrs{BucketName: "fixtures", Name: "hello.txt"},
			Content:     []byte("hello"),
		}},
		NoListener: true,
	})
	if err != nil {
		log.Fatal(err)
	}
	defer server.Stop()

	reader, err := server.Client().Bucket("fixtures").Object("hello.txt").NewReader(context.Background())
	if err != nil {
		log.Fatal(err)
	}
	defer reader.Close()
	body, err := io.ReadAll(reader)
	if err != nil {
		log.Fatal(err)
	}
	fmt.Println(string(body))
}
