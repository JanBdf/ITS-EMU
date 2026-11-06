/**
 * Emulator test program, built from the course's lwIP demo (Programs/Stack):
 * the address from DHCP, then echo servers for TCP and UDP on port 7.
 * Lines on the virtual COM port:
 *   N ip <address>        once DHCP has supplied the address
 *   N tcp <bytes>         data echoed on a connection
 */
#include <stdio.h>
#include "stm32f4xx_hal.h"

#include "lwip/dhcp.h"
#include "lwip/init.h"
#include "lwip/ip4_addr.h"
#include "lwip/netif.h"
#include "lwip/tcp.h"
#include "lwip/timeouts.h"
#include "lwip/udp.h"
#include "net/ethernetif.h"
#include "netif/ethernet.h"

extern void initITSboard(void);
extern ETH_HandleTypeDef EthHandle;

static struct netif nif;

static err_t echo_recv(void *arg, struct tcp_pcb *pcb, struct pbuf *p, err_t err) {
    (void)arg;
    (void)err;
    if (p == NULL) {
        tcp_close(pcb);
        return ERR_OK;
    }
    for (struct pbuf *q = p; q != NULL; q = q->next) {
        tcp_write(pcb, q->payload, q->len, TCP_WRITE_FLAG_COPY);
    }
    tcp_output(pcb);
    tcp_recved(pcb, p->tot_len);
    printf("N tcp %u\n", p->tot_len);
    pbuf_free(p);
    return ERR_OK;
}

static err_t echo_accept(void *arg, struct tcp_pcb *pcb, err_t err) {
    (void)arg;
    (void)err;
    tcp_recv(pcb, echo_recv);
    return ERR_OK;
}

static void udp_echo(void *arg, struct udp_pcb *pcb, struct pbuf *p, const ip_addr_t *addr, u16_t port) {
    (void)arg;
    udp_sendto(pcb, p, addr, port);
    pbuf_free(p);
}

int main(void) {
    initITSboard();
    lwip_init();
    netif_add(&nif, IP4_ADDR_ANY4, IP4_ADDR_ANY4, IP4_ADDR_ANY4, NULL, ethernetif_init, ethernet_input);
    netif_set_default(&nif);
    netif_set_up(&nif);
    dhcp_start(&nif);

    struct tcp_pcb *listener = tcp_new();
    tcp_bind(listener, IP_ADDR_ANY, 7);
    listener = tcp_listen(listener);
    tcp_accept(listener, echo_accept);
    struct udp_pcb *u = udp_new();
    udp_bind(u, IP_ADDR_ANY, 7);
    udp_recv(u, udp_echo, NULL);
    printf("N start\n");

    int announced = 0;
    while (1) {
        if ((EthHandle.RxDesc->Status & ETH_DMARXDESC_OWN) == 0) {
            ethernetif_input(&nif);
        }
        sys_check_timeouts();
        if (!announced && dhcp_supplied_address(&nif)) {
            printf("N ip %s\n", ip4addr_ntoa(netif_ip4_addr(&nif)));
            announced = 1;
        }
    }
}
