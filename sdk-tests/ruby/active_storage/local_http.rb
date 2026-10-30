# frozen_string_literal: true

require "ipaddr"
require "net/http"
require "uri"

# Restricts every Net::HTTP connection to the emulator origin, so an OAuth or
# metadata-server request fails before reaching the network.
module LocalHTTP
  def self.origin(raw)
    uri = URI.parse(raw)
    unless uri.is_a?(URI::HTTP) && uri.scheme == "http" && uri.userinfo.nil? &&
           [nil, "", "/"].include?(uri.path) && uri.query.nil? && uri.fragment.nil? &&
           (uri.hostname == "localhost" || IPAddr.new(uri.hostname).loopback?)
      raise ArgumentError, "GCS_MIMIC_ENDPOINT must be a loopback HTTP origin, e.g. http://127.0.0.1:4443"
    end
    uri
  rescue URI::InvalidURIError, IPAddr::InvalidAddressError
    raise ArgumentError, "invalid loopback HTTP origin; use http://127.0.0.1:4443"
  end

  def self.validate!(uri)
    expected = origin(ENV.fetch("GCS_MIMIC_ENDPOINT"))
    unless [uri.scheme, uri.hostname, uri.port] == [expected.scheme, expected.hostname, expected.port]
      raise ArgumentError, "request targets #{uri.scheme}://#{uri.host}:#{uri.port}; use local credentials and endpoint (e.g. http://127.0.0.1:4443/); see sdk-active-storage-scenario.md"
    end
  end

  def self.install!
    origin(ENV.fetch("GCS_MIMIC_ENDPOINT"))
    %w[http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY].each { |key| ENV.delete(key) }
    Net::HTTP.prepend(self)
  end

  def start(*)
    LocalHTTP.validate!(URI("#{use_ssl? ? 'https' : 'http'}://#{addr_port}"))
    self.ipaddr = "127.0.0.1" if address == "localhost"
    self.open_timeout = self.read_timeout = self.write_timeout = 10
    super
  end

  def request(req, *args, &block)
    response = super
    if response.code.to_i >= 400
      warn "WIRE #{req.method} #{req.path.split('?').first} -> #{response.code}: #{response.body.to_s.byteslice(0, 500)}"
    end
    response
  end
end
