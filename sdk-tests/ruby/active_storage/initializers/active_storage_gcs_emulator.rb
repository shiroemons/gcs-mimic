# frozen_string_literal: true

# An application copies this file to config/initializers/. strict.rb loads it only after its
# unpatched cases, so that they still observe the stock URL.
#
# Active Storage builds the URLs of a public: true GCS service with the SDK, which always names
# storage.googleapis.com. In development and test, a service whose storage.yml entry sets an
# endpoint, such as the gcs-mimic entry, names that endpoint instead. Entries without an endpoint,
# such as production's, keep the stock URL.
if Rails.env.local?
  require "active_storage/service/gcs_service"
  require "erb"

  module GcsEmulatorPublicUrl
    private

    def public_url(key, **)
      endpoint = config[:endpoint]
      return super if endpoint.blank?

      path = key.split("/", -1).map { |segment| ERB::Util.url_encode(segment) }.join("/")
      "#{endpoint.delete_suffix('/')}/#{bucket.name}/#{path}"
    end
  end

  ActiveStorage::Service::GCSService.prepend(GcsEmulatorPublicUrl)
end
