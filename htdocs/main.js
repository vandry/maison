const {Climate, MonitorEverythingRequest, SetLightsRequest} = require('codegen/maison_pb.js');
const {MaisonClient} = require('codegen/maison_grpc_web_pb.js');

class Maison {
    constructor (api) {
        this.api = api;
        this.staleness_expiries = {};
        this.kitchen = [null, null, null];
        this.garden_lights = null;
        this.garden_lights_until = null;
        this.garden_lights_refresh = null;
    }

    run = () => {
        var top = this;
        var req = new MonitorEverythingRequest();
        if (document.getElementsByClassName("livetemp").length > 0) {
            req.setWantLiveTemperatures(true);
        }
        if (document.getElementsByClassName("kitchen_lights").length > 0) {
            req.setWantKitchenCeiling(true);
            req.setWantKitchenUnderCupboards(true);
            req.setWantKitchenUnderStairs(true);
        }
        if (
            (document.getElementsByClassName("heating").length > 0) ||
            (document.getElementsByClassName("hot_water").length > 0)
        ) {
            req.setWantBoiler(true);
        }
        var garden_lights_els = document.getElementsByClassName("garden_lights");
        for (var i = 0; i < garden_lights_els.length; i++) {
            req.setWantGardenLights(true);
            garden_lights_els[i].addEventListener('click', () => {
                var req = new SetLightsRequest();
                if (top.garden_lights) {
                    req.setDurationMs(0);
                } else {
                    req.setDurationMs(600000);
                }
                this.api.setGardenLights(req, {}, (err, response) => {
                    if (err) {
                        console.log(err.code + " " + err.message);
                    }
                });
            });
        }
        if (document.getElementsByClassName("garden_lights_timer").length > 0) {
            req.setWantMaison(true);
        }
        this.subscribe(() => {
            return this.api.monitorEverything(req, {});
        }, (response) => {
            if (response === null) {
                top.accept_livetemp(null);
                top.set_kitchen(null, 0);
                top.set_kitchen(null, 1);
                top.set_kitchen(null, 2);
                top.accept_boiler(null);
                top.accept_garden_lights(null);
                top.accept_maison(null);
                return;
            }
            if (response.hasLiveTemperature()) {
                top.accept_livetemp(response.getLiveTemperature());
            }
            if (response.hasKitchenCeiling()) {
                top.set_kitchen(response.getKitchenCeiling(), 0);
            }
            if (response.hasKitchenUnderCupboards()) {
                top.set_kitchen(response.getKitchenUnderCupboards(), 1);
            }
            if (response.hasKitchenUnderStairs()) {
                top.set_kitchen(response.getKitchenUnderStairs(), 2);
            }
            if (response.hasBoiler()) {
                top.accept_boiler(response.getBoiler());
            }
            if (response.hasGardenLights()) {
                top.accept_garden_lights(response.getGardenLights());
            }
            if (response.hasMaison()) {
                top.accept_maison(response.getMaison());
            }
        });
    }

    display_value = (staleness_key, value, lifetime, setter) => {
        if (this.staleness_expiries.hasOwnProperty(staleness_key)) {
            clearTimeout(this.staleness_expiries[staleness_key]);
            delete this.staleness_expiries[staleness_key];
        }
        setter(value);
        if ((value !== null) && (lifetime !== null)) {
            this.staleness_expiries[staleness_key] = setTimeout(() => {
                this.display_value(staleness_key, null, null, setter);
            }, lifetime);
        }
    }

    subscribe = (make_stream, callback) => {
        var unhealthy = [null];
        this.resubscribe(make_stream, callback, unhealthy);
    }

    resubscribe = (make_stream, callback, unhealthy) => {
        var top = this;
        var restart = [null];
        var stream = make_stream();
        stream.on('data', (response) => {
            if (unhealthy[0] !== null) {
                clearTimeout(unhealthy[0]);
                unhealthy[0] = null;
            }
            callback(response);
        });
        stream.on('status', function(status) {
            if (unhealthy[0] === null) {
                unhealthy[0] = setTimeout(() => { callback(null); }, 5000);
            }
            if (restart[0] === null) {
                restart[0] = setTimeout(() => { top.resubscribe(make_stream, callback, unhealthy); }, 1000);
            }
        });
        stream.on('end', function() {
            if (unhealthy[0] === null) {
                unhealthy[0] = setTimeout(() => { callback(null); }, 5000);
            }
            if (restart[0] === null) {
                restart[0] = setTimeout(() => { top.subscribe(make_stream, callback, unhealthy); }, 0);
            }
        });
    }

    accept_livetemp = (response) => {
        var key = (response === null) ? 'temperatures' : ('climate-' + response.getUnit());
        this.display_value(
            key,
            (response === null) ? null : (response.hasTemperature() ? response.getTemperature() : null),
            4500000,
            (v) => {
                var els = document.getElementsByClassName(key);
                for (var i = 0; i < els.length; i++) {
                    var livetemp_els = els[i].getElementsByClassName("livetemp");
                    for (var j = 0; j < livetemp_els.length; j++) {
                        livetemp_els[j].textContent = v === null ? '' : (v.toFixed(1) + " \xb0C");
                    }
                }
            },
        );
    }

    set_kitchen = (response, light_index) => {
        var top = this;
        this.display_value(
                'kitchen_' + light_index,
                (response === null) ? null : (response.hasState() ? response.getState() : null),
                66000000,
                (v) => {
                    top.kitchen[light_index] = v;
                    var known = top.kitchen[0] !== null && top.kitchen[1] !== null && top.kitchen[2] !== null;
                    var on = known && top.kitchen[0] && top.kitchen[1] && top.kitchen[2];
                    var off = known && (!top.kitchen[0]) && (!top.kitchen[1]) && (!top.kitchen[2]);
                    var overall = known ? (on ? "light_on" : (off ? "light_off" : "light_some")) : "light_unknown";
                    var els = document.getElementsByClassName("kitchen_lights");
                    for (var i = 0; i < els.length; i++) {
                        var spans = els[i].getElementsByTagName("span");
                        for (var j = 0; j < spans.length; j++) {
                            spans[j].className = overall;
                        }
                    }
                },
        );
    }

    accept_boiler = (response) => {
        this.display_value(
            'live_heating',
            (response === null) ? null : (response.hasHeating() ? response.getHeating() : null),
            66000000,
            (v) => {
                var els = document.getElementsByClassName("heating");
                for (var i = 0; i < els.length; i++) {
                    var spans = els[i].getElementsByTagName("span");
                    for (var j = 0; j < spans.length; j++) {
                        spans[j].className = (v === null) ? 'light_unknown' : (v ? 'light_on' : 'light_off');
                    }
                }
            },
        );
        this.display_value(
            'live_hot_water',
            (response === null) ? null : (response.hasHotWater() ? response.getHotWater() : null),
            66000000,
            (v) => {
                var els = document.getElementsByClassName("hot_water");
                for (var i = 0; i < els.length; i++) {
                    var spans = els[i].getElementsByTagName("span");
                    for (var j = 0; j < spans.length; j++) {
                        spans[j].className = (v === null) ? 'light_unknown' : (v ? 'light_on' : 'light_off');
                    }
                }
            },
        );
    }

    accept_garden_lights = (response) => {
        var top = this;
        this.display_value(
            'garden_lights',
            (response === null) ? null : (response.hasState() ? response.getState() : null),
            66000000,
            (v) => {
                top.garden_lights = v;
                var els = document.getElementsByClassName("garden_lights");
                for (var i = 0; i < els.length; i++) {
                    var spans = els[i].getElementsByTagName("span");
                    for (var j = 0; j < spans.length; j++) {
                        spans[j].className = (v === null) ? 'light_unknown' : (v ? 'light_on' : 'light_off');
                    }
                }
            },
        );
    }

    accept_maison = (response) => {
        var countdown = null;
        if ((response !== null) && response.hasGardenLightUntil()) {
            var seconds = response.getGardenLightUntil().getSeconds() - response.getNow().getSeconds();
            var nanos = response.getGardenLightUntil().getNanos() - response.getNow().getNanos();
            if (nanos < 0) {
                nanos += 1000000000;
                seconds -= 1;
            }
            this.garden_lights_until = Date.now() + (seconds * 1000) + (nanos / 1000000);
        } else {
            this.garden_lights_until = null;
        }
        this.garden_lights_update();
        if ((this.garden_lights_until !== null) && (this.garden_lights_refresh === null)) {
            var top = this;
            this.garden_lights_refresh = setInterval(() => { top.garden_lights_update() }, 500);
        }
    }

    garden_lights_update = () => {
        var v = this.garden_lights_until;
        var countdown = "";
        if (v === null) {
            if (this.garden_lights_refresh !== null) {
                clearInterval(this.garden_lights_refresh);
                this.garden_lights_refresh = null;
                this.garden_lights_until = null;
            }
        } else {
            var remaining = v - Date.now();
            if (remaining < 0) {
                if (this.garden_lights_refresh !== null) {
                    clearInterval(this.garden_lights_refresh);
                    this.garden_lights_refresh = null;
                    this.garden_lights_until = null;
                }
            } else {
                remaining -= remaining % 1000;
                if (remaining < 60000) {
                    countdown = (remaining / 1000).toFixed(0) + "s";
                } else {
                    var ms = remaining % 60000;
                    var m = (remaining - ms) / 60000;
                    if (ms < 10000) {
                        countdown = m + "m0" + (ms / 1000).toFixed(0) + "s";
                    } else {
                        countdown = m + "m" + (ms / 1000).toFixed(0) + "s";
                    }
                }
            }
        }
        var els = document.getElementsByClassName("garden_lights_timer");
        for (var i = 0; i < els.length; i++) {
            els[i].textContent = countdown;
        }
    }
}

new Maison(new MaisonClient(URL.parse(window.location).toString())).run();
