# frozen_string_literal: true

require "active_storage/service/gcs_service"
require "uri"

# An explicit local adapter: the SDK's endpoint override does not change its V4 canonical Host.
class ActiveStorage::Service::EmulatorGcsService < ActiveStorage::Service::GCSService
  def initialize(public_endpoint:, **config)
    @public_endpoint = URI(public_endpoint)
    unless @public_endpoint.is_a?(URI::HTTP) && @public_endpoint.host &&
           @public_endpoint.userinfo.nil? && ["", "/"].include?(@public_endpoint.path) &&
           @public_endpoint.query.nil? && @public_endpoint.fragment.nil?
      raise ArgumentError, "public_endpoint must be a bare final HTTP(S) origin"
    end
    raise ArgumentError, "the local signing adapter requires local service-account credentials" if config[:iam]
    super(**config)
  end

  # Signs the size limit Rails 8.1.4.1 sends (GHSA-hfr4-8rjc-97m2), as the stock V2 URL does.
  def url_for_direct_upload(key, expires_in:, checksum:, content_length:, custom_metadata: {}, **)
    headers = custom_metadata_headers(custom_metadata)
    headers["x-goog-content-length-range"] = content_length_range(content_length)
    headers["Content-MD5"] = checksum
    headers["Cache-Control"] = config[:cache_control] if config[:cache_control].present?
    signing_file(key).signed_url(**signing_options, version: :v4, method: "PUT",
      expires: expires_in, headers: headers)
  end

  private

    def private_url(key, expires_in:, filename:, content_type:, disposition:, **)
      signing_file(key).signed_url(**signing_options, version: :v4, method: "GET", expires: expires_in,
        query: {"response-content-disposition" => content_disposition_with(type: disposition, filename: filename),
                "response-content-type" => content_type})
    end

    def signing_file(key)
      # Bucket-bound mode omits the bucket path; include it before the SDK signs the final URI.
      bucket.file("#{bucket.name}/#{key}", skip_lookup: true)
    end

    def signing_options
      host = @public_endpoint.host
      host = "#{host}:#{@public_endpoint.port}" unless @public_endpoint.port == @public_endpoint.default_port
      {scheme: @public_endpoint.scheme, bucket_bound_hostname: host}
    end
end
