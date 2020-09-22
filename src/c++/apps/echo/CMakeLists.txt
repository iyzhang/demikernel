# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

cmake_minimum_required (VERSION 3.5.1)
project (EchoServer C CXX)

#set(RUNTIME_OUTPUT_DIRECTORY "execs")

set(THREADS_PREFER_PTHREAD_FLAG ON)

find_package(Threads REQUIRED)
find_package(Boost REQUIRED COMPONENTS
  program_options chrono context system coroutine)
include_directories(${Boost_INCLUDE_DIR})

set(ECHO_APPS_DIR ${CMAKE_CURRENT_SOURCE_DIR})

set(TCP_ECHO_SERVER_SOURCES ${ECHO_APPS_DIR}/dmtr_tcp_server.cc)
set(TCP_ECHO_CLIENT_SOURCES ${ECHO_APPS_DIR}/dmtr_tcp_client.cc)
set(UDP_ECHO_SERVER_SOURCES ${ECHO_APPS_DIR}/dmtr_udp_server.cc)
set(UDP_ECHO_CLIENT_SOURCES ${ECHO_APPS_DIR}/dmtr_udp_client.cc)
set(RAW_ECHO_SERVER_SOURCES ${ECHO_APPS_DIR}/posix_tcp_server.cc)
set(RAW_ECHO_CLIENT_SOURCES ${ECHO_APPS_DIR}/posix_tcp_client.cc)
set(STORE_TEST_SOURCES ${ECHO_APPS_DIR}/dmtr_file_test.cc)

# POSIX TCP server
add_executable(dmtr-posix-server ${TCP_ECHO_SERVER_SOURCES})
target_link_libraries(dmtr-posix-server dmtr-libos-posix yaml-cpp boost_program_options)

# POSIX TCP client
add_executable(dmtr-posix-client ${TCP_ECHO_CLIENT_SOURCES})
target_link_libraries(dmtr-posix-client dmtr-libos-posix yaml-cpp boost_program_options)

# POSIX TCP client & server (works with storage too)
add_custom_target(dmtr-posix-echo)
add_dependencies(dmtr-posix-echo  dmtr-posix-server dmtr-posix-client)

# LWIP TCP server
add_executable(dmtr-lwip-server ${TCP_ECHO_SERVER_SOURCES})
target_link_libraries(dmtr-lwip-server dmtr-libos-lwip yaml-cpp boost_program_options)

# LWIP TCP client
add_executable(dmtr-lwip-client  ${TCP_ECHO_CLIENT_SOURCES})
target_link_libraries(dmtr-lwip-client dmtr-libos-lwip yaml-cpp boost_program_options)

# LWIP TCP client & server
add_custom_target(dmtr-lwip-echo)
add_dependencies(dmtr-lwip-echo dmtr-lwip-server dmtr-lwip-client)

# LWIP & SPDK client
add_executable(dmtr-spdk-lwip-client  ${TCP_ECHO_CLIENT_SOURCES})
target_link_libraries(dmtr-spdk-lwip-client dmtr-libos-lwip yaml-cpp boost_program_options)

# LWIP & SPDK server
add_executable(dmtr-spdk-lwip-server ${TCP_ECHO_SERVER_SOURCES})
target_link_libraries(dmtr-spdk-lwip-server dmtr-libos-spdk-dpdk yaml-cpp boost_program_options)

# LWIP & SPDK client & server
add_custom_target(dmtr-spdk-lwip-echo)
add_dependencies(dmtr-spdk-lwip-echo dmtr-spdk-lwip-server dmtr-spdk-lwip-client)

# RDMA server
add_executable(dmtr-rdma-server ${TCP_ECHO_SERVER_SOURCES})
target_link_libraries(dmtr-rdma-server dmtr-libos-rdma rdmacm ibverbs yaml-cpp boost_program_options)

# RDMA client
add_executable(dmtr-rdma-client ${TCP_ECHO_CLIENT_SOURCES})
target_link_libraries(dmtr-rdma-client dmtr-libos-rdma rdmacm ibverbs yaml-cpp boost_program_options)

# RDMA client & server
add_custom_target(dmtr-rdma-echo)
add_dependencies(dmtr-rdma-echo dmtr-rdma-server dmtr-rdma-client)

# RDMA & SPDK client
add_executable(dmtr-spdk-rdma-client  ${TCP_ECHO_CLIENT_SOURCES})
target_link_libraries(dmtr-spdk-rdma-client dmtr-libos-rdma yaml-cpp boost_program_options)

# RDMA & SPDK server
add_executable(dmtr-spdk-rdma-server ${TCP_ECHO_SERVER_SOURCES})
target_link_libraries(dmtr-spdk-rdma-server dmtr-libos-spdk-rdma yaml-cpp boost_program_options)

# RDMA & SPDK client and server
add_custom_target(dmtr-spdk-rdma-echo)
add_dependencies(dmtr-spdk-rdma-echo dmtr-spdk-rdma-server dmtr-spdk-rdma-client)


# POSIX "raw" non-demeter server
add_executable(posix-server ${RAW_ECHO_SERVER_SOURCES})
target_link_libraries(posix-server dmtr-latency dmtr-libos-common yaml-cpp boost_program_options)

# POSIX "raw" non-demeter client
add_executable(posix-client ${RAW_ECHO_CLIENT_SOURCES})
target_link_libraries(posix-client dmtr-latency dmtr-libos-common yaml-cpp boost_program_options)

# POSIX "raw" client & server
add_custom_target(posix-echo)
add_dependencies(posix-echo posix-server posix-client)

add_custom_target(echo)
add_dependencies(echo posix-echo dmtr-posix-echo dmtr-lwip-echo dmtr-rdma-echo dmtr-dpdk-catnip-echo)

# DPDK+catnip TCP server
add_executable(dmtr-dpdk-catnip-server ${TCP_ECHO_SERVER_SOURCES})
target_link_libraries(dmtr-dpdk-catnip-server dmtr-libos-dpdk-catnip yaml-cpp boost_program_options)

# DPDK+catnip TCP client
add_executable(dmtr-dpdk-catnip-client  ${TCP_ECHO_CLIENT_SOURCES})
target_link_libraries(dmtr-dpdk-catnip-client dmtr-libos-dpdk-catnip yaml-cpp boost_program_options)

# DPDK+catnip UDP server
add_executable(dmtr-dpdk-catnip-udp-server ${UDP_ECHO_SERVER_SOURCES})
target_link_libraries(dmtr-dpdk-catnip-udp-server dmtr-libos-dpdk-catnip yaml-ccp boost_program_options)

# DPDK+catnip UDP client
add_executable(dmtr-dpdk-catnip-udp-client ${UDP_ECHO_CLIENT_SOURCES})
target_link_libraries(dmtr-dpdk-catnip-udp-client dmtr-libos-dpdk-catnip yaml-ccp boost_program_options)

# DPDK+catnip TCP client & server
add_custom_target(dmtr-dpdk-catnip-echo)
add_dependencies(dmtr-dpdk-catnip-echo dmtr-dpdk-catnip-server dmtr-dpdk-catnip-client)

# DPDK+catnip UDP client & server
add_custom_target(dmtr-dpdk-catnip-udp-echo)
add_dependencies(dmtr-dpdk-catnip-udp-echo dmtr-dpdk-catnip-udp-server dmtr-dpdk-catnip-udp-client)

# POSIX & SPDK storage test clients
# POSIX TCP client & server
add_custom_target(dmtr-echo-store)
add_dependencies(dmtr-echo-store  dmtr-posix-echo dmtr-spdk-rdma-echo dmtr-spdk-lwip-echo posix-echo)
