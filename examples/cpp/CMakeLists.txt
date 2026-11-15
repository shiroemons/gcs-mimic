cmake_minimum_required(VERSION 3.20)
project(gcs_mimic_example_cpp CXX)

# The SDK documents C++17; GCC 16 would otherwise default to C++20.
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

find_package(google_cloud_cpp_storage REQUIRED)

add_executable(gcs-mimic-example-cpp main.cc)
target_compile_options(gcs-mimic-example-cpp PRIVATE -Wall -Wextra -Wpedantic)
target_link_libraries(gcs-mimic-example-cpp PRIVATE google-cloud-cpp::storage)

install(TARGETS gcs-mimic-example-cpp RUNTIME DESTINATION bin)
