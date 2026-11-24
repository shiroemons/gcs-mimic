# syntax=docker/dockerfile:1@sha256:572cebbc2b15e6f84c077742952674889cb63cd5e9b80abdffcbe587b6f19183
# Builds the official C++ Cloud Storage SDK probe. Build with the scorecard directory as the
# context: docker build -f sdk-scorecard/cpp/Dockerfile sdk-scorecard
# The official gcc image already carries the libcurl and OpenSSL development packages, so the
# only OS package pinned here is socat; every library is a checksum-verified source archive.
FROM gcc:16.2.0-trixie@sha256:e39cfa939c18b4dcb1bcf77cdae11a2d8b2dcfbee0eb531c24694334047509f8
ARG TARGETARCH
# GCC 16 defaults to C++20; the SDK documents and tests C++17.
ARG CXX_STD=17

# Both archives are verified even though only the build architecture's one is unpacked, so a
# changed upstream file fails every build instead of only the other architecture's.
ADD --checksum=sha256:3bec208f11ed96c173032eeaf97937c54dd246acefee22acd7794a6b4e70d536 https://github.com/Kitware/CMake/releases/download/v4.5.1/cmake-4.5.1-linux-aarch64.tar.gz /dl/cmake-arm64.tar.gz
ADD --checksum=sha256:83b71181fb62b4fe8f26067a9321448572e7a43bd04ff6c7f54a24c33698f16d https://github.com/Kitware/CMake/releases/download/v4.5.1/cmake-4.5.1-linux-x86_64.tar.gz /dl/cmake-amd64.tar.gz
RUN mkdir -p /opt/cmake \
 && tar -xzf "/dl/cmake-${TARGETARCH}.tar.gz" -C /opt/cmake --strip-components=1 \
 && rm /dl/cmake-*.tar.gz
ENV PATH=/opt/cmake/bin:$PATH

ADD --checksum=sha256:f7e05179df39c45434cad433f5783840bb3788ef322976f9138bc6b72b3a107d https://github.com/abseil/abseil-cpp/releases/download/20260817.0/abseil-cpp-20260817.0.tar.gz /dl/abseil-cpp.tar.gz
RUN mkdir /src && tar -xzf /dl/abseil-cpp.tar.gz -C /src --strip-components=1 \
 && cmake -S /src -B /build -DCMAKE_BUILD_TYPE=Release -DCMAKE_CXX_STANDARD=${CXX_STD} \
      -DABSL_BUILD_TESTING=OFF -DABSL_PROPAGATE_CXX_STD=ON -DBUILD_SHARED_LIBS=OFF \
      -DCMAKE_POSITION_INDEPENDENT_CODE=ON -DCMAKE_INSTALL_PREFIX=/usr/local \
 && cmake --build /build -j"$(nproc)" && cmake --install /build \
 && rm -rf /src /build /dl/abseil-cpp.tar.gz

ADD --checksum=sha256:42f6e95cad6ec532fd372391373363b62a14af6d771056dbfc86160e6dfff7aa https://github.com/nlohmann/json/releases/download/v3.12.0/json.tar.xz /dl/nlohmann-json.tar.xz
RUN mkdir /src && tar -xJf /dl/nlohmann-json.tar.xz -C /src --strip-components=1 \
 && cmake -S /src -B /build -DCMAKE_BUILD_TYPE=Release -DJSON_BuildTests=OFF -DCMAKE_INSTALL_PREFIX=/usr/local \
 && cmake --build /build && cmake --install /build \
 && rm -rf /src /build /dl/nlohmann-json.tar.xz

# google_cloud_cpp_common 3.14.1 includes <opentelemetry/...> headers unconditionally, even with
# the opentelemetry feature disabled; the header-only API satisfies them without the SDK.
ADD --checksum=sha256:85aae7a42d7b71d17e510ddebc85db622d9078cc6d29cf9fb7373014b063859f https://github.com/open-telemetry/opentelemetry-cpp/archive/refs/tags/v1.30.0.tar.gz /dl/opentelemetry-cpp.tar.gz
RUN mkdir /src && tar -xzf /dl/opentelemetry-cpp.tar.gz -C /src --strip-components=1 \
 && cmake -S /src -B /build -DCMAKE_BUILD_TYPE=Release -DCMAKE_CXX_STANDARD=${CXX_STD} -DOTELCPP_WITH_API_ONLY=ON \
      -DBUILD_TESTING=OFF -DWITH_EXAMPLES=OFF -DCMAKE_INSTALL_PREFIX=/usr/local \
 && cmake --build /build && cmake --install /build \
 && rm -rf /src /build /dl/opentelemetry-cpp.tar.gz

# "storage" alone also enables monitoring, trace, opentelemetry and universe_domain
# (cmake/GoogleCloudCppFeatures.cmake), which require gRPC and Protobuf; the REST client does not.
ADD --checksum=sha256:e8ed19ddb68755332262b613dea3f98d2a044fceecd7a60d862523b6eba84daf https://github.com/googleapis/google-cloud-cpp/archive/refs/tags/v3.14.1.tar.gz /dl/google-cloud-cpp.tar.gz
RUN mkdir /src && tar -xzf /dl/google-cloud-cpp.tar.gz -C /src --strip-components=1 \
 && cmake -S /src -B /build -DCMAKE_BUILD_TYPE=Release -DCMAKE_CXX_STANDARD=${CXX_STD} \
      -DBUILD_SHARED_LIBS=OFF -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      "-DGOOGLE_CLOUD_CPP_ENABLE=storage,-monitoring,-trace,-opentelemetry,-universe_domain" \
      -DBUILD_TESTING=OFF -DGOOGLE_CLOUD_CPP_WITH_MOCKS=OFF -DGOOGLE_CLOUD_CPP_ENABLE_EXAMPLES=OFF \
      -DCMAKE_INSTALL_PREFIX=/usr/local \
 && cmake --build /build -j"$(nproc)" && cmake --install /build \
 && rm -rf /src /build /dl/google-cloud-cpp.tar.gz

# The probe keeps loopback URLs inside the container; socat forwards them to the host, as for the
# Rust probe. Installed after the libraries so a socat pin change does not rebuild them.
RUN --mount=type=bind,source=install-socat.sh,target=/app/sdk-scorecard/install-socat.sh \
    /app/sdk-scorecard/install-socat.sh

# The probe is built last so editing it reuses every cached library layer. It is C++17 like the
# SDK and installed where run.sh and test-tls.sh expect it.
COPY cpp/CMakeLists.txt /probe/CMakeLists.txt
COPY cpp/src /probe/src
RUN cmake -S /probe -B /probe/build -DCMAKE_BUILD_TYPE=Release -DCMAKE_CXX_STANDARD=${CXX_STD} \
      -DCMAKE_INSTALL_PREFIX=/usr/local \
 && cmake --build /probe/build -j"$(nproc)" && cmake --install /probe/build \
 && rm -rf /probe
