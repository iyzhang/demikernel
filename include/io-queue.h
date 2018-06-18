// -*- mode: c++; c-file-style: "k&r"; c-basic-offset: 4 -*-
/***********************************************************************
 *
 * include/io-queue.h
 *   Zeus io-queue interface
 *
 * Copyright 2018 Irene Zhang  <irene.zhang@microsoft.com>
 *
 * Permission is hereby granted, free of charge, to any person
 * obtaining a copy of this software and associated documentation
 * files (the "Software"), to deal in the Software without
 * restriction, including without limitation the rights to use, copy,
 * modify, merge, publish, distribute, sublicense, and/or sell copies
 * of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be
 * included in all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
 * EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
 * MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
 * NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS
 * BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN
 * ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
 * CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 *
 **********************************************************************/
 
#ifndef _IO_QUEUE_H_
#define _IO_QUEUE_H_

#include <sys/types.h>
#include <sys/socket.h>
#include <memory>
    
#define MAX_QUEUE_DEPTH 40
#define MAX_SGARRAY_SIZE 10

#define ZEUS_IO_ERR_NO (-9)

namespace Zeus {
    
typedef void * ioptr;
typedef int qtoken;    
    
struct sgelem {
    ioptr buf;
    size_t len;
};
    
struct sgarray {
    int num_bufs;
    sgelem bufs[MAX_SGARRAY_SIZE];
};

// memory allocation
//ioptr iomalloc(size_t size);

// network functions
int queue(int domain, int type, int protocol);
int listen(int qd, int backlog);
int bind(int qd, struct sockaddr *saddr, socklen_t size);
int accept(int qd, struct sockaddr *saddr, socklen_t *size);
int connect(int qd, struct sockaddr *saddr, socklen_t size);
int close(int qd);
          
// file functions
int open(const char *pathname, int flags);
int open(const char *pathname, int flags, mode_t mode);
int creat(const char *pathname, mode_t mode);

// other functions
qtoken push(int qd, struct sgarray &sga); // if return 0, then already complete
qtoken pop(int qd, struct sgarray &sga); // if return 0, then already ready and in sga
ssize_t wait_any(qtoken qts[], size_t num_qts);
ssize_t wait_all(qtoken qts[], size_t num_qts);
// identical to a push, followed by a wait on the returned qtoken
ssize_t blocking_push(int qd, struct sgarray &sga);
// identical to a pop, followed by a wait on the returned qtoken
ssize_t blocking_pop(int qd, struct sgarray &sga); 

// returns the file descriptor associated with
// the queue descriptor if the queue is an io queue
int qd2fd(int qd);

// eventually queue functions
int merge(int qd1, int qd2);
// int filter(int qd, bool (*filter)(struct sgarray &sga));

} // namespace Zeus
#endif /* _IO_QUEUE_H_ */