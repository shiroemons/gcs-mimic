# frozen_string_literal: true

require "rails"
require "active_record/railtie"
require "active_storage/engine"

module StorageScenario
  class Application < Rails::Application
    config.load_defaults 8.1
    config.eager_load = false
    config.secret_key_base = "local-active-storage-scenario-only-" * 4
    config.logger = Logger.new($stderr)
    config.log_level = :warn
    # Applications select gcs_emulator per environment; each script picks the service under test.
    config.active_storage.service = ENV.fetch("ACTIVE_STORAGE_SERVICE", "local_gcs").to_sym
    config.active_storage.analyzers = []
    config.active_storage.previewers = []
    config.active_storage.variant_processor = :disabled
    config.active_job.queue_adapter = :inline
    config.hosts = ["127.0.0.1"]
  end
end
