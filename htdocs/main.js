const {Climate, IncrementSetPointRequest, MonitorEverythingRequest, SetLightsRequest, SetManyRequest} = require('codegen/maison_pb.js');
const {MaisonClient} = require('codegen/maison_grpc_web_pb.js');

function find_climate_zone(e) {
    while (e.className.substr(0, 8) != "climate-") {
        e = e.parentNode;
        if (e === null) {
            return null;
        }
    }
    return e.className.substr(8);
}

function get_clock_angle(e) {
    var x = (e.offsetX / e.target.width) - 0.5;
    var y = (e.offsetY / e.target.height) - 0.5;
    var slice = Math.round(Math.atan2(y, x) * 6 / Math.PI) + 3;
    if (slice < 0) {
        return slice + 12;
    }
    return slice;
}

class ClockSelector {
    constructor (hel, mel, actuator) {
        this.actuator = actuator;
        this.hel = hel;
        this.mel = mel;
        this.hours = 0;
    }

    run = () => {
        this.mel.style.display = "none";
        var top = this;
        this.hel.addEventListener('click', (e) => {
            top.hours = get_clock_angle(e);
            top.hel.style.display = "none";
            top.mel.style.display = "inherit";
        });
        this.mel.addEventListener('click', (e) => {
            var req = new SetLightsRequest();
            req.setDurationMs(top.hours * 3600000 + get_clock_angle(e) * 300000);
            top.hel.style.display = "inherit";
            top.mel.style.display = "none";
            top.hel.parentNode.style.display = "none";
            top.actuator(req, {}, (err, response) => {
                if (err) {
                    console.log(err.code + " " + err.message);
                }
            });
        });
    }
}

function install_timer_adjust(cl, actuator) {
    var els = document.getElementsByClassName(cl);
    for (var i = 0; i < els.length; i++) {
        var hours_el = null;
        var minutes_el = null;
        for (var j = 0; j < els[i].childNodes.length; j++) {
            var el = els[i].childNodes[j];
            if (el.tagName === "IMG") {
                if (hours_el === null) {
                    hours_el = el;
                } else {
                    minutes_el = el;
                }
            }
        }
        if ((hours_el !== null) && (minutes_el !== null)) {
            new ClockSelector(hours_el, minutes_el, actuator).run();
        }
    }
}

class Maison {
    constructor (api) {
        this.api = api;
        this.staleness_expiries = {};
        this.kitchen = [null, null, null];
        this.garden_lights = null;
        this.garden_lights_until = null;
        this.garden_lights_refresh = null;
        this.heating_override_until = null;
        this.heating_override_refresh = null;
        this.hot_water_override_until = null;
        this.hot_water_override_refresh = null;
        this.clock_offset = null;
        this.clock_refresh = null;
        this.livetemp = {};
        this.setpoint = {};
    }

    override_set_point_request = (zone) => {
        var req = new IncrementSetPointRequest();
        req.setZone(zone);
        if (this.livetemp.hasOwnProperty(zone)) {
            req.setStartingValueIfUnset(Math.round(this.livetemp[zone] * 2) / 2);
        }
        req.setDurationMsIfNotAlreadySet(7200000);
        return req;
    }

    mkcolder = (z) => {
        var top = this;
        return () => {
            var req = top.override_set_point_request(z);
            req.setIncrement(-0.5);
            top.api.incrementSetPoint(req, {}, (err, response) => {
                if (err) {
                    console.log(err.code + " " + err.message);
                }
            });
        };
    }

    mkwarmer = (z) => {
        var top = this;
        return () => {
            var req = top.override_set_point_request(z);
            req.setIncrement(0.5);
            top.api.incrementSetPoint(req, {}, (err, response) => {
                if (err) {
                    console.log(err.code + " " + err.message);
                }
            });
        };
    }

    install_react_hot_water = (cl) => {
        var els = document.getElementsByClassName(cl);
        var top = this;
        for (var i = 0; i < els.length; i++) {
            els[i].addEventListener('click', (e) => {
                var req = new SetLightsRequest();
                if (e.target.parentNode.className === "light_on") {
                    req.setDurationMs(0);
                } else {
                    req.setDurationMs(7200000);
                }
                top.api.setHotWater(req, {}, (err, response) => {
                    if (err) {
                        console.log(err.code + " " + err.message);
                    }
                });
            });
        }
    }

    install_react_heating = (cl) => {
        var els = document.getElementsByClassName(cl);
        var top = this;
        for (var i = 0; i < els.length; i++) {
            els[i].addEventListener('click', (e) => {
                if (e.target.parentNode.className !== "light_on") {
                    return;
                }
                top.api.cancelHeatingOverride(new proto.google.protobuf.Empty(), {}, (err, response) => {
                    if (err) {
                        console.log(err.code + " " + err.message);
                    }
                });
            });
        }
    }

    run = () => {
        var top = this;
        var req = new MonitorEverythingRequest();
        if (document.getElementsByClassName("livetemp").length > 0) {
            req.setWantLiveTemperatures(true);
        }
        var colder_els = document.getElementsByClassName("colder");
        for (var i = 0; i < colder_els.length; i++) {
            var z = find_climate_zone(colder_els[i]);
            if (z === null) {
                continue;
            }
            colder_els[i].addEventListener('click', this.mkcolder(z));
        }
        var warmer_els = document.getElementsByClassName("warmer");
        for (var i = 0; i < warmer_els.length; i++) {
            var z = find_climate_zone(warmer_els[i]);
            if (z === null) {
                continue;
            }
            warmer_els[i].addEventListener('click', this.mkwarmer(z));
        }
        this.install_react_hot_water("hot_water");
        this.install_react_hot_water("hot_water_override");
        this.install_react_heating("heating_override");
        var kitchen_lights_els = document.getElementsByClassName("kitchen_lights");
        for (var i = 0; i < kitchen_lights_els.length; i++) {
            req.setWantKitchenCeiling(true);
            req.setWantKitchenUnderCupboards(true);
            req.setWantKitchenUnderStairs(true);
            kitchen_lights_els[i].addEventListener('click', () => {
                var req = new SetManyRequest();
                var all_known = (top.kitchen[0] !== null) && (top.kitchen[1] !== null) && (top.kitchen[2] !== null);
                if (all_known && top.kitchen[0] && top.kitchen[1] && top.kitchen[2]) {
                    req.setKitchenCeiling(false);
                    req.setKitchenUnderCupboards(false);
                    req.setKitchenUnderStairs(false);
                } else if (all_known && (!top.kitchen[0]) && (!top.kitchen[1]) && (!top.kitchen[2])) {
                    req.setKitchenCeiling(true);
                    req.setKitchenUnderCupboards(true);
                    req.setKitchenUnderStairs(true);
                } else {
                    var els = document.getElementsByClassName("kitchen_popup");
                    for (var i = 0; i < els.length; i++) {
                        els[i].style.display = "inherit";
                    }
                    return;
                }
                this.api.setMany(req, {}, (err, response) => {
                    if (err) {
                        console.log(err.code + " " + err.message);
                    }
                });
            });
        }
        var kitchen_ceiling_els = document.getElementsByClassName("kitchen_ceiling");
        for (var i = 0; i < kitchen_ceiling_els.length; i++) {
            req.setWantKitchenCeiling(true);
            kitchen_ceiling_els[i].addEventListener('click', () => {
                var req = new SetManyRequest();
                if (top.kitchen[0]) {
                    req.setKitchenCeiling(false);
                } else {
                    req.setKitchenCeiling(true);
                }
                this.api.setMany(req, {}, (err, response) => {
                    if (err) {
                        console.log(err.code + " " + err.message);
                    }
                });
            });
        }
        var kitchen_under_cupboards_els = document.getElementsByClassName("kitchen_under_cupboards");
        for (var i = 0; i < kitchen_under_cupboards_els.length; i++) {
            req.setWantKitchenUnderCupboards(true);
            kitchen_under_cupboards_els[i].addEventListener('click', () => {
                var req = new SetManyRequest();
                if (top.kitchen[1]) {
                    req.setKitchenUnderCupboards(false);
                } else {
                    req.setKitchenUnderCupboards(true);
                }
                this.api.setMany(req, {}, (err, response) => {
                    if (err) {
                        console.log(err.code + " " + err.message);
                    }
                });
            });
        }
        var kitchen_under_stairs_els = document.getElementsByClassName("kitchen_under_stairs");
        for (var i = 0; i < kitchen_under_stairs_els.length; i++) {
            req.setWantKitchenUnderStairs(true);
            kitchen_under_stairs_els[i].addEventListener('click', () => {
                var req = new SetManyRequest();
                if (top.kitchen[2]) {
                    req.setKitchenUnderStairs(false);
                } else {
                    req.setKitchenUnderStairs(true);
                }
                this.api.setMany(req, {}, (err, response) => {
                    if (err) {
                        console.log(err.code + " " + err.message);
                    }
                });
            });
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
        var hot_water_timer_els = document.getElementsByClassName("hot_water_timer");
        for (var i = 0; i < hot_water_timer_els.length; i++) {
            req.setWantMaison(true);
            hot_water_timer_els[i].addEventListener('click', () => {
                var els = document.getElementsByClassName("hot_water_timer_popup");
                for (var i = 0; i < els.length; i++) {
                    els[i].style.display = "inherit";
                }
            });
        }
        var heating_timer_els = document.getElementsByClassName("heating_timer");
        for (var i = 0; i < heating_timer_els.length; i++) {
            req.setWantMaison(true);
            heating_timer_els[i].addEventListener('click', () => {
                var els = document.getElementsByClassName("heating_timer_popup");
                for (var i = 0; i < els.length; i++) {
                    els[i].style.display = "inherit";
                }
            });
        }
        if (
            (document.getElementsByClassName("garden_lights_timer").length > 0) ||
            (document.getElementsByClassName("heating_override").length > 0) ||
            (document.getElementsByClassName("hot_water_override").length > 0) ||
            (document.getElementsByClassName("clock").length > 0)
        ) {
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
        var closer_els = document.getElementsByClassName("closer");
        for (var i = 0; i < closer_els.length; i++) {
            for (var j = 0; j < closer_els[i].childNodes.length; j++) {
                closer_els[i].childNodes[j].addEventListener('click', (e) => {
                    e.target.parentNode.parentNode.style.display = "none";
                });
            }
        }
        var kitchen_details_els = document.getElementsByClassName("kitchen_details");
        for (var i = 0; i < kitchen_details_els.length; i++) {
            kitchen_details_els[i].addEventListener('click', () => {
                var els = document.getElementsByClassName("kitchen_popup");
                for (var i = 0; i < els.length; i++) {
                    els[i].style.display = "inherit";
                }
            });
        }
        install_timer_adjust("hot_water_timer_popup", (r, h, c) => { top.api.setHotWater(r, h, c); });
        install_timer_adjust("heating_timer_popup", (r, h, c) => { top.api.setHeating(r, h, c); });
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
        if (response === null) {
            this.livetemp = {};
        } else if (response.hasUnit()) {
            if (response.hasTemperature()) {
                this.livetemp[response.getUnit()] = response.getTemperature();
            } else {
                delete this.livetemp[response.getUnit()];
            }
        }
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
                    var overall0 = (top.kitchen[0] === null) ? "light_unknown" : (top.kitchen[0] ? "light_on" : "light_off");
                    var overall1 = (top.kitchen[1] === null) ? "light_unknown" : (top.kitchen[1] ? "light_on" : "light_off");
                    var overall2 = (top.kitchen[2] === null) ? "light_unknown" : (top.kitchen[2] ? "light_on" : "light_off");
                    var els = document.getElementsByClassName("kitchen_lights");
                    for (var i = 0; i < els.length; i++) {
                        var spans = els[i].getElementsByTagName("span");
                        for (var j = 0; j < spans.length; j++) {
                            spans[j].className = overall;
                        }
                    }
                    var els0 = document.getElementsByClassName("kitchen_ceiling");
                    for (var i = 0; i < els0.length; i++) {
                        var spans = els0[i].getElementsByTagName("span");
                        for (var j = 0; j < spans.length; j++) {
                            spans[j].className = overall0;
                        }
                    }
                    var els1 = document.getElementsByClassName("kitchen_under_cupboards");
                    for (var i = 0; i < els1.length; i++) {
                        var spans = els1[i].getElementsByTagName("span");
                        for (var j = 0; j < spans.length; j++) {
                            spans[j].className = overall1;
                        }
                    }
                    var els2 = document.getElementsByClassName("kitchen_under_stairs");
                    for (var i = 0; i < els2.length; i++) {
                        var spans = els2[i].getElementsByTagName("span");
                        for (var j = 0; j < spans.length; j++) {
                            spans[j].className = overall2;
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

    get_timer = (v) => {
        var ms = v.getSeconds() * 1000 + v.getNanos() / 1e6;
        return ms + this.clock_offset;
    }

    accept_maison = (response) => {
        if (response === null) {
            this.garden_lights_until = null;
            this.heating_override_until = null;
            this.hot_water_override_until = null;
            this.clock_offset = null;
            if (this.clock_refresh !== null) {
                clearTimeout(this.clock_refresh);
                this.clock_refresh = null;
                this.update_clock();
            }
            this.setpoint = {};
        } else {
            var now = Date.now();
            var server_now = response.getNow().getSeconds() * 1000 + response.getNow().getNanos() / 1e6;
            this.clock_offset = server_now - now;
            if (this.clock_refresh === null) {
                this.update_clock();
            }
            if (response.hasGardenLightUntil()) {
                this.garden_lights_until = this.get_timer(response.getGardenLightUntil());
            } else {
                this.garden_lights_until = null;
            }
            if (response.hasHeatingOverrideUntil()) {
                this.heating_override_until = this.get_timer(response.getHeatingOverrideUntil());
            } else {
                this.heating_override_until = null;
            }
            if (response.hasHotWaterOverrideUntil()) {
                this.hot_water_override_until = this.get_timer(response.getHotWaterOverrideUntil());
            } else {
                this.hot_water_override_until = null;
            }
            var setpoint = {};
            var hol = response.getHeatingOverrideList();
            for (var i = 0; i < hol.length; i++) {
                var sp = hol[i];
                if (sp.hasZone() && sp.hasSetpoint()) {
                    setpoint[sp.getZone()] = sp.getSetpoint();
                }
            }
            this.setpoint = setpoint;
        }
        this.garden_lights_update();
        this.heating_update();
        this.hot_water_update();
        if ((this.garden_lights_until !== null) && (this.garden_lights_refresh === null)) {
            var top = this;
            this.garden_lights_refresh = setInterval(() => { top.garden_lights_update() }, 500);
        }
        if ((this.hot_water_override_until !== null) && (this.hot_water_override_refresh === null)) {
            var top = this;
            this.hot_water_override_refresh = setInterval(() => { top.hot_water_update() }, 30000);
        }
        if ((this.heating_override_until !== null) && (this.heating_override_refresh === null)) {
            var top = this;
            this.heating_override_refresh = setInterval(() => { top.heating_update() }, 30000);
        }
    }

    update_clock = () => {
        var els = document.getElementsByClassName("clock");
        if (els.length < 1) {
            return;
        }
        var t = "";
        if (this.clock_offset !== null) {
            var dnow = new Date();
            var now = dnow.getTime() + this.clock_offset;
            var tzoffset_now = now - dnow.getTimezoneOffset() * 60000;
            var hack = new Date(tzoffset_now);
            var s = hack.toISOString();
            t = s.substr(0, 10) + " " + s.substr(11, 5);
            var top = this;
            this.clock_refresh = setTimeout(() => {
                top.update_clock();
            }, 60000 - (now % 60000));
        }
        for (var i = 0; i < els.length; i++) {
            els[i].textContent = t;
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

    hot_water_update = () => {
        var v = this.hot_water_override_until;
        var countdown = "";
        if (v === null) {
            if (this.hot_water_override_refresh !== null) {
                clearInterval(this.hot_water_override_refresh);
                this.hot_water_override_refresh = null;
                this.hot_water_override_until = null;
            }
        } else {
            var remaining = v - Date.now();
            if (remaining < 0) {
                if (this.hot_water_override_refresh !== null) {
                    clearInterval(this.hot_water_override_refresh);
                    this.hot_water_override_refresh = null;
                    this.hot_water_override_refresh = null;
                }
                v = null;
                this.hot_water_override_until = null;
            } else {
                remaining -= remaining % 60000;
                if (remaining < 3600000) {
                    countdown = (remaining / 60000).toFixed(0) + "min.";
                } else {
                    var hm = remaining % 3600000;
                    var h = (remaining - hm) / 3600000;
                    if (hm < 600000) {
                        countdown = h + "h0" + (hm / 60000).toFixed(0);
                    } else {
                        countdown = h + "h" + (hm / 60000).toFixed(0);
                    }
                }
            }
        }
        var els1 = document.getElementsByClassName("hot_water_timer");
        for (var i = 0; i < els1.length; i++) {
            els1[i].textContent = countdown;
        }
        var els2 = document.getElementsByClassName("hot_water_override");
        for (var i = 0; i < els2.length; i++) {
            var spans = els2[i].getElementsByTagName("span");
            for (var j = 0; j < spans.length; j++) {
                spans[j].className = (v === null) ? "light_off" : "light_on";
            }
        }
    }

    heating_update = () => {
        var v = this.heating_override_until;
        var countdown = "";
        if (v === null) {
            if (this.heating_override_refresh !== null) {
                clearInterval(this.heating_override_refresh);
                this.heating_override_refresh = null;
                this.heating_override_until = null;
                this.setpoint = {};
            }
        } else {
            var remaining = v - Date.now();
            if (remaining < 0) {
                if (this.heating_override_refresh !== null) {
                    clearInterval(this.heating_override_refresh);
                    this.heating_override_refresh = null;
                    this.heating_override_refresh = null;
                }
                v = null;
                this.heating_override_until = null;
                this.setpoint = {};
            } else {
                remaining -= remaining % 60000;
                if (remaining < 3600000) {
                    countdown = (remaining / 60000).toFixed(0) + "min.";
                } else {
                    var hm = remaining % 3600000;
                    var h = (remaining - hm) / 3600000;
                    if (hm < 600000) {
                        countdown = h + "h0" + (hm / 60000).toFixed(0);
                    } else {
                        countdown = h + "h" + (hm / 60000).toFixed(0);
                    }
                }
            }
        }
        var els1 = document.getElementsByClassName("heating_timer");
        for (var i = 0; i < els1.length; i++) {
            els1[i].textContent = countdown;
        }
        var els2 = document.getElementsByClassName("heating_override");
        for (var i = 0; i < els2.length; i++) {
            var spans = els2[i].getElementsByTagName("span");
            for (var j = 0; j < spans.length; j++) {
                spans[j].className = (v === null) ? "light_off" : "light_on";
            }
        }
        var els3 = document.getElementsByClassName("setpoint");
        for (var i = 0; i < els3.length; i++) {
            var zone = find_climate_zone(els3[i]);
            if (zone === null) {
                continue;
            }
            if (this.setpoint.hasOwnProperty(zone)) {
                els3[i].textContent = this.setpoint[zone].toFixed(1);
            } else {
                els3[i].textContent = "";
            }
        }
    }
}

new Maison(new MaisonClient(URL.parse(window.location).toString())).run();
