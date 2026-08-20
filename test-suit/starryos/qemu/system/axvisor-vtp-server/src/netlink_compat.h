/*
 * Minimal Linux netlink UAPI definitions for the Starry VTP server.
 *
 * The CI cross-compilation container does not ship linux-libc-dev, so
 * <linux/netlink.h> / <linux/rtnetlink.h> are unavailable to the grouped C
 * build. This header provides the small, stable subset of the netlink/rtnetlink
 * UAPI that the server needs, keeping it self-contained and buildable anywhere.
 *
 * All values follow the long-stable kernel UAPI and must stay in sync with it:
 * they are not re-invented here, only restated for a header-less environment.
 */

#ifndef NETLINK_COMPAT_H
#define NETLINK_COMPAT_H

#include <stdint.h>
#include <sys/socket.h> /* sa_family_t */

#define AF_NETLINK       16
#define NETLINK_ROUTE     0

#define NLM_F_REQUEST  0x0001
#define NLM_F_ACK      0x0004
#define NLM_F_REPLACE  0x0100
#define NLM_F_CREATE   0x0400

#define RTM_NEWADDR       20
#define RT_SCOPE_UNIVERSE  0

#define IFA_F_PERMANENT  0x80
#define IFA_ADDRESS      1
#define IFA_LOCAL        2

struct nlmsghdr {
    uint32_t nlmsg_len;
    uint16_t nlmsg_type;
    uint16_t nlmsg_flags;
    uint32_t nlmsg_seq;
    uint32_t nlmsg_pid;
};

struct ifaddrmsg {
    uint8_t ifa_family;
    uint8_t ifa_prefixlen;
    uint8_t ifa_flags;
    uint8_t ifa_scope;
    uint32_t ifa_index;
};

struct nlattr {
    uint16_t nla_len;
    uint16_t nla_type;
};

struct sockaddr_nl {
    sa_family_t nl_family;
    unsigned short nl_pad;
    uint32_t nl_pid;
    uint32_t nl_groups;
};

#define NLMSG_ALIGNTO   4U
#define NLMSG_ALIGN(len) (((len) + NLMSG_ALIGNTO - 1) & ~(NLMSG_ALIGNTO - 1))
#define NLMSG_HDRLEN    ((int)NLMSG_ALIGN(sizeof(struct nlmsghdr)))
#define NLMSG_LENGTH(len) ((len) + NLMSG_HDRLEN)
#define NLMSG_DATA(nlh) ((void *)((char *)(nlh) + NLMSG_LENGTH(0)))

#define NLA_ALIGNTO     4
#define NLA_ALIGN(len)  (((len) + NLA_ALIGNTO - 1) & ~(NLA_ALIGNTO - 1))
#define NLA_HDRLEN      ((int)NLA_ALIGN(sizeof(struct nlattr)))

#endif /* NETLINK_COMPAT_H */
