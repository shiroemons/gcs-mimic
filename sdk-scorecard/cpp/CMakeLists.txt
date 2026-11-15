cmake_minimum_required(VERSION 3.20)
project(gcs_mimic_scorecard_cpp CXX)

# The SDK documents C++17; GCC 16 would otherwise default to C++20.
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

find_package(google_cloud_cpp_storage REQUIRED)
find_package(nlohmann_json REQUIRED)
find_package(CURL REQUIRED)
find_package(ZLIB REQUIRED)

add_executable(gcs-mimic-scorecard-cpp
  src/cases_bucket.cc
  src/cases_object.cc
  src/cases_sdk.cc
  src/cases_signed.cc
  src/config.cc
  src/harness.cc
  src/main.cc
  src/runner.cc
  src/sdk_log.cc
  src/signed_http.cc
  src/transport_check.cc)
target_compile_options(gcs-mimic-scorecard-cpp PRIVATE -Wall -Wextra -Wpedantic)
target_link_libraries(gcs-mimic-scorecard-cpp PRIVATE
  google-cloud-cpp::storage nlohmann_json::nlohmann_json CURL::libcurl ZLIB::ZLIB)

install(TARGETS gcs-mimic-scorecard-cpp RUNTIME DESTINATION bin)
