# frozen_string_literal: true

require "stringio"

def migration_scenario(storage)
  bucket = storage.bucket("assets")
  raise "buckets file did not initialize assets" unless bucket

  name = "uploads/ruby-migration.txt"
  payload = "Ruby migration bytes\n"
  object = nil
  begin
    raise "migration object already exists" if bucket.file(name)

    object = bucket.create_file(StringIO.new(payload), name)
    found = bucket.file(name)
    unless found && found.generation == object.generation && found.size == payload.bytesize && found.content_type == "text/plain"
      raise "migration bucket.file metadata differs"
    end
    downloaded = found.download
    begin
      raise "migration downloaded bytes differ" unless downloaded.read(payload.bytesize + 1) == payload
    ensure
      downloaded.close
    end
    object.delete
    object = nil
    raise "deleted migration object still exists" if bucket.file(name)

    puts "ruby-migration-scenario passed: anonymous endpoint, initialized bucket, create_file, bucket.file, bytes and delete"
  ensure
    object&.delete
  end
end
